//! The filter expression language (DESIGN §7).
//!
//! `genre="jazz"|genre="blues"`, `artist="*ferrell*"&year>2020`.
//!
//! Terse on purpose — this is typed at a `:` prompt, not written in prose.
//! Strings match by glob with `*` as the only metacharacter, because music
//! metadata is made of regex metacharacters and regex fails *silently* on it:
//! see the Decision Log for the counts that decided this.

use crate::format::{Field, Kind};
use crate::store::Track;

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

        if !case_sensitive {
            for s in &mut segments {
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

    pub fn matches(&self, text: &str) -> bool {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    Field(Field),
    Any,
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

    pub fn compile(&self) -> impl Fn(&Track) -> bool + '_ {
        self.expr.compile()
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
}

fn field_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Field::NAMES.to_vec();
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
    pub fn compile(&self) -> impl Fn(&Track) -> bool + '_ {
        move |track| self.eval(track)
    }

    fn eval(&self, t: &Track) -> bool {
        match self {
            Expr::And(a, b) => a.eval(t) && b.eval(t),
            Expr::Or(a, b) => a.eval(t) || b.eval(t),
            Expr::Not(e) => !e.eval(t),
            Expr::Present(Selector::Field(f)) => present(*f, t),
            Expr::Present(Selector::Any) => true,
            Expr::Cmp {
                selector,
                op,
                value,
            } => compare(*selector, *op, value, t),
        }
    }
}

/// Text fields `any` looks at.
///
/// Not `path`: every track's path contains the library root, so including it
/// would make `any="*music*"` match the entire library.
const ANY_FIELDS: &[Field] = &[
    Field::Artist,
    Field::AlbumArtist,
    Field::Album,
    Field::Title,
    Field::Genre,
    Field::FileName,
];

fn compare(selector: Selector, op: Op, value: &Value, t: &Track) -> bool {
    match (selector, value) {
        (Selector::Any, Value::Text(glob)) => {
            let hit = ANY_FIELDS
                .iter()
                .any(|f| text_of(*f, t).is_some_and(|v| glob.matches(&v)));
            match op {
                Op::Ne => !hit,
                _ => hit,
            }
        }
        (Selector::Field(f), Value::Text(glob)) => match text_of(f, t) {
            // A missing tag matches nothing, including `!=`. Three-valued
            // logic would be more correct and much harder to predict; `!` on
            // the whole comparison is how you ask for "or missing".
            Some(v) => match op {
                Op::Ne => !glob.matches(&v),
                _ => glob.matches(&v),
            },
            None => false,
        },
        (Selector::Field(f), Value::Number(n)) => match number_of(f, t) {
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
fn present(f: Field, t: &Track) -> bool {
    match f.kind() {
        Kind::Text => text_of(f, t).is_some_and(|v| !v.trim().is_empty()),
        Kind::Number => number_of(f, t).is_some(),
    }
}

fn text_of(f: Field, t: &Track) -> Option<String> {
    Some(match f {
        Field::Artist => t.artist.clone()?,
        Field::AlbumArtist => t.albumartist.clone()?,
        Field::FileUnder => t.file_under().to_string(),
        Field::Album => t.album.clone()?,
        Field::Title => t.title.clone()?,
        Field::DisplayTitle => t.display_title().to_string(),
        Field::Genre => t.genre.clone()?,
        Field::Path => t.path.display().to_string(),
        Field::FileName => t.path.file_name()?.to_string_lossy().into_owned(),
        _ => return None,
    })
}

fn number_of(f: Field, t: &Track) -> Option<i64> {
    Some(match f {
        Field::Track => i64::from(t.tracknumber?),
        Field::Disc => i64::from(t.discnumber?),
        Field::Year => i64::from(t.date?),
        // Seconds, not the `3:41` the format language renders.
        Field::Duration => t.duration.as_secs() as i64,
        Field::Bitrate => i64::from(t.bitrate?),
        Field::SampleRate => i64::from(t.samplerate?),
        Field::Channels => i64::from(t.channels?),
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

        let selector = if name == "any" {
            Selector::Any
        } else {
            Selector::Field(
                Field::parse(&name).ok_or(FilterError::UnknownField { name: name.clone() })?,
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
            Selector::Field(f) => f.kind(),
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
    use std::path::PathBuf;
    use std::time::Duration;

    fn track() -> Track {
        Track {
            path: PathBuf::from("/m/Sierra Ferrell/Long Time Coming/01 The Sea.mp3"),
            artist: Some("Sierra Ferrell".into()),
            albumartist: Some("Sierra Ferrell".into()),
            album: Some("Long Time Coming".into()),
            title: Some("The Sea".into()),
            genre: Some("folk".into()),
            tracknumber: Some(1),
            date: Some(2021),
            duration: Duration::from_secs(221),
            bitrate: Some(320),
            ..Default::default()
        }
    }

    fn matches(expr: &str, t: &Track) -> bool {
        Expr::parse(expr).expect("should parse").compile()(t)
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
    #[test]
    fn a_missing_tag_matches_nothing() {
        let mut t = track();
        t.genre = None;
        t.date = None;
        assert!(!matches(r#"genre="folk""#, &t));
        assert!(!matches(r#"genre!="folk""#, &t));
        assert!(!matches("year>1900", &t));
        assert!(matches(r#"!genre="folk""#, &t), "negation still works");
    }

    /// `field?` asks whether the track has the field at all — the one thing
    /// foobar2000 could say and this language could only imply.
    #[test]
    fn presence_and_absence() {
        let full = track();
        let mut bare = track();
        bare.genre = None;
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
        t.genre = Some(String::new());
        assert!(!matches("genre?", &t));
        t.genre = Some("   ".into());
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
}
