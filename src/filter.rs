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
    /// Every claim about this recording on one axis, as `(label, basis,
    /// confidence)` — #93's correlated paths.
    ///
    /// **One tuple per assertion**, which is the whole point: the model keys
    /// `(subject, concept, basis) -> confidence`, so "Discogs said techno,
    /// provisionally" is one row and asking about it needs the row rather than
    /// three independent sets.
    fn claims(&self, recording: Id<Recording>, facet_key: &str) -> Vec<(String, String, String)>;

    /// What this recording was **derived from**, bounded — #79.
    ///
    /// One tuple per edge on the way up, `(kind, depth)`, where depth is hops
    /// from this recording: a remix of it is 1, a remix of that remix is 2.
    /// Correlated for the same reason [`Self::claims`] is — "a remix at most
    /// two hops back" is one edge satisfying both, not two independent tests.
    ///
    /// **The bound is not optional.** Edges are rows and rows union, so two
    /// devices can produce a cycle neither authored, and an unbounded walk
    /// hangs a render rather than drawing a wrong one.
    fn derived_from(&self, recording: Id<Recording>) -> Vec<(String, i64)>;

    /// The same walk over **works** rather than recordings — the other
    /// traversal, one level up.
    ///
    /// Separate and it does not reach into the other: recording derivations
    /// answer "which versions", work derivations answer "which arrangements",
    /// and a query crossing between them would answer a question nobody asked.
    fn work_derived_from(&self, recording: Id<Recording>) -> Vec<(String, i64)>;

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
    /// The pattern when it has no wildcard in it, so a caller can name the
    /// value a fact would carry (#96).
    ///
    /// `None` for anything with a `*`: "assign genre `*tech*` to this" is not
    /// a fact anybody can write, and the caller refuses rather than guessing
    /// which concept was meant.
    pub fn literal(&self) -> Option<String> {
        (self.anchored_start && self.anchored_end && self.segments.len() == 1)
            .then(|| self.segments[0].clone())
    }

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
    /// Which of #96's three categories this is.
    pub fn authorship(self) -> crate::format::Authorship {
        use crate::format::Authorship;
        match self {
            // A judgement, and the one this player exists to collect.
            HistoryField::Rating => Authorship::Authored,
            // What is in a crate is authored, and `:collect` is how.
            HistoryField::Collected => Authorship::Authored,
            // What happened. A play count is not an opinion.
            HistoryField::PlayCount
            | HistoryField::DaysSincePlayed
            | HistoryField::DaysSinceAdded => Authorship::Observed,
        }
    }

    pub const NAMES: &'static [&'static str] = &[
        "collected",
        "days_since_added",
        "days_since_played",
        "play_count",
        "rating",
    ];

    /// What it is spelled as, which is the inverse of [`Self::parse`].
    pub fn name(self) -> &'static str {
        match self {
            HistoryField::Collected => "collected",
            HistoryField::PlayCount => "play_count",
            HistoryField::DaysSincePlayed => "days_since_played",
            HistoryField::DaysSinceAdded => "days_since_added",
            HistoryField::Rating => "rating",
        }
    }

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

/// One attribute of a single claim — what a bracket asks about (#93).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The concept itself: `genre[label="techno"]`.
    Label,
    /// Who said so — `tag`, `inferred`, `musicbrainz`, `discogs`, `manual`.
    Basis,
    /// How sure they were — `provisional`, `matched`, `confirmed`.
    Confidence,
}

/// What a derivation bracket can ask about one hop — #79.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derived {
    /// `derived[kind=remix]` — what sort of derivation it was.
    Kind,
    /// `derived[depth<=2]` — how many hops back, a remix of a remix being 2.
    ///
    /// **The bound the model already enforces, said as a question.** A walk is
    /// capped at 16 whatever anyone asks, because edges union and two devices
    /// can author a cycle neither meant.
    Depth,
}

impl Derived {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "kind" => Derived::Kind,
            "depth" => Derived::Depth,
            _ => return None,
        })
    }

    fn name(self) -> String {
        match self {
            Derived::Kind => "kind".to_string(),
            Derived::Depth => "depth".to_string(),
        }
    }
}

impl Claim {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "label" => Claim::Label,
            "basis" => Claim::Basis,
            "confidence" => Claim::Confidence,
            _ => return None,
        })
    }

    pub const NAMES: &'static [&'static str] = &["basis", "confidence", "label"];
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// `genre[basis=discogs & confidence!=confirmed]` — **one claim satisfies
    /// all of these** (#93).
    ///
    /// The bracket is the correlation scope, which is XPath's answer and the
    /// one three traditions converge on: SQL correlates because `WHERE` runs
    /// per joined row, SPARQL because a variable names the node, XPath because
    /// of where the brackets go. **Correlation requires naming the thing being
    /// correlated**, and this is the name.
    ///
    /// A conjunction only, and that is not a limitation: ∃x(P∨Q) ≡ ∃xP ∨ ∃xQ,
    /// so `genre[a] | genre[b]` says everything `genre[a | b]` would. It is
    /// exactly ∃x(P∧Q) that does *not* distribute, which is why the bracket is
    /// needed at all.
    Claims {
        facet: &'static str,
        of: Vec<(Claim, Op, Value)>,
    },
    /// `derived[kind=remix & depth<=2]` — a bounded walk up a derivation
    /// graph, #79 and `domain-model.md` §5.
    ///
    /// **The same correlated bracket as [`Expr::Claims`]**, applied to the
    /// second relation the model has: one *edge* must satisfy every clause, so
    /// "a remix at most two hops back" cannot be answered by a remix somewhere
    /// and something two hops away.
    ///
    /// An empty bracket is "derived from anything", which is the question
    /// `domain-model.md` writes as `derived_from(rec)`.
    Derivation {
        /// Works rather than recordings — the other traversal, one level up.
        works: bool,
        of: Vec<(Derived, Op, Value)>,
    },
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
    #[error("{name:?} is not an axis — `genre[…]` asks about a genre's claims")]
    NotAnAxis { name: String },
    #[error(
        "{name:?} is not something a claim carries — try one of: {}",
        Claim::NAMES.join(", ")
    )]
    NotAClaim { name: String },
    #[error("{facet}[…] needs something inside it")]
    EmptyBracket { facet: &'static str },
    #[error("{facet}[…] is missing its closing bracket")]
    UnclosedBracket { facet: &'static str },
    #[error(
        "`|` inside a bracket says nothing extra — `genre[a] | genre[b]` is the same question, and `&` is the one that needs the bracket"
    )]
    OrInBracket,
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
        // **The memo belongs to the compiled filter, not to a row** (#93).
        //
        // Closure costs a hierarchy walk, and measured it was 60 ms over 20k
        // rows at depth 12 — sixty times the flat language and far too slow
        // for a filter that runs on every keystroke. The cost is per *label*
        // rather than per row, and a library has a few hundred distinct
        // genres over thousands of tracks, so the second occurrence of a
        // genre is a hash lookup.
        //
        // Owned by the closure, so it lives exactly as long as the filter is
        // in use and cannot go stale against a refold: a new fold compiles a
        // new filter.
        let memo = Memo::default();
        move |subject| self.eval(subject, &env, &memo)
    }

    fn eval(&self, t: &Subject<'_>, env: &Env, memo: &Memo) -> bool {
        match self {
            Expr::And(a, b) => a.eval(t, env, memo) && b.eval(t, env, memo),
            Expr::Or(a, b) => a.eval(t, env, memo) || b.eval(t, env, memo),
            Expr::Not(e) => !e.eval(t, env, memo),
            Expr::Present(Selector::Field(_, _, f)) => present(*f, t),
            Expr::Present(Selector::Any) => true,
            Expr::Present(Selector::History(h)) => history_number(*h, t, env).is_some(),
            Expr::Cmp {
                selector,
                op,
                value,
            } => compare(*selector, *op, value, t, env, memo),
            Expr::Derivation { works, of } => {
                let (Some(recording), Some(listened)) = (t.recording, env.listened) else {
                    return false;
                };
                let edges = if *works {
                    listened.work_derived_from(recording)
                } else {
                    listened.derived_from(recording)
                };
                edges.iter().any(|(kind, depth)| {
                    of.iter().all(|(what, op, value)| match (what, value) {
                        (Derived::Kind, Value::Text(glob)) => {
                            let hit = glob.matches(kind);
                            if *op == Op::Ne { !hit } else { hit }
                        }
                        (Derived::Depth, Value::Number(n)) => compare_numbers(*op, *depth, *n),
                        // A kind is a word and a depth is a number; the parser
                        // refuses the other pairings, so nothing reaches here.
                        _ => false,
                    })
                })
            }
            Expr::Claims { facet, of } => {
                let (Some(recording), Some(listened)) = (t.recording, env.listened) else {
                    return false;
                };
                listened
                    .claims(recording, facet)
                    .iter()
                    .any(|(label, basis, confidence)| {
                        of.iter().all(|(what, op, value)| {
                            let held = match what {
                                Claim::Label => label,
                                Claim::Basis => basis,
                                Claim::Confidence => confidence,
                            };
                            let Value::Text(glob) = value else {
                                return false;
                            };
                            match op {
                                Op::Ne => !glob.matches(held),
                                _ => glob.matches(held),
                            }
                        })
                    })
            }
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

/// One numeric comparison, the six operators.
fn compare_numbers(op: Op, held: i64, want: i64) -> bool {
    match op {
        Op::Eq => held == want,
        Op::Ne => held != want,
        Op::Lt => held < want,
        Op::Le => held <= want,
        Op::Gt => held > want,
        Op::Ge => held >= want,
    }
}

fn days_since(then: i64, now: i64) -> i64 {
    (now - then).max(0) / 86_400
}

impl Parser {
    /// The inside of a `[…]`: comparisons joined by `&`.
    ///
    /// **`|` is refused, and nothing is lost by it.** ∃x(P∨Q) ≡ ∃xP ∨ ∃xQ, so
    /// `genre[a] | genre[b]` says everything `genre[a | b]` would — where
    /// ∃x(P∧Q) does *not* distribute, which is the entire reason the bracket
    /// exists. Refusing it keeps one way to write each meaning.
    /// The inside of a `derived[…]` — #79.
    ///
    /// Its own parser rather than [`Self::claims`]'s because the two brackets
    /// admit different things: a claim is compared for equality only, and a
    /// **depth is ordered** — `depth<=2` is the whole point of the bound being
    /// expressible. An empty bracket is "derived from anything".
    fn derivation(&mut self, works: bool) -> Result<Expr, FilterError> {
        let what = if works { "work_derived" } else { "derived" };
        let mut of = Vec::new();
        if self.peek() == Some(&Token::CloseBracket) {
            self.next();
            return Ok(Expr::Derivation { works, of });
        }
        loop {
            let name = match self.next() {
                Some(Token::Word(w)) => w,
                _ => return Err(FilterError::EmptyBracket { facet: what }),
            };
            let clause = Derived::parse(&name).ok_or(FilterError::NotAClaim { name })?;
            let op = match self.next() {
                Some(Token::Op(op)) => op,
                _ => return Err(FilterError::ExpectedOperator(clause.name())),
            };
            // A kind is a word, so ordering it means nothing; a depth is a
            // number, and ordering it is the reason the clause exists.
            if clause == Derived::Kind && !matches!(op, Op::Eq | Op::Ne) {
                return Err(FilterError::NotOrderable {
                    field: clause.name(),
                    op: "an ordering",
                });
            }
            let value = match (clause, self.next()) {
                (Derived::Depth, Some(Token::Word(w))) => match w.parse::<i64>() {
                    Ok(n) => Value::Number(n),
                    Err(_) => return Err(FilterError::NotANumber(w)),
                },
                (Derived::Kind, Some(Token::Word(w) | Token::Quoted(w))) => {
                    Value::Text(Glob::new(&w))
                }
                _ => return Err(FilterError::ExpectedOperator(clause.name())),
            };
            of.push((clause, op, value));
            match self.next() {
                Some(Token::CloseBracket) => break,
                Some(Token::And) => {}
                Some(Token::Or) => return Err(FilterError::OrInBracket),
                _ => return Err(FilterError::UnclosedBracket { facet: what }),
            }
        }
        Ok(Expr::Derivation { works, of })
    }

    fn claims(&mut self, facet: &'static str) -> Result<Expr, FilterError> {
        let mut of = Vec::new();
        loop {
            let name = match self.next() {
                Some(Token::Word(w)) => w,
                _ => return Err(FilterError::EmptyBracket { facet }),
            };
            let what = Claim::parse(&name).ok_or(FilterError::NotAClaim { name })?;
            let op = match self.next() {
                Some(Token::Op(op)) => op,
                _ => return Err(FilterError::ExpectedOperator(name_of(what))),
            };
            if !matches!(op, Op::Eq | Op::Ne) {
                return Err(FilterError::NotOrderable {
                    field: name_of(what),
                    op: "an ordering",
                });
            }
            let value = match self.next() {
                Some(Token::Word(w)) => Value::Text(Glob::new(&w)),
                Some(Token::Quoted(q)) => Value::Text(Glob::new(&q)),
                _ => return Err(FilterError::ExpectedOperator(name_of(what))),
            };
            of.push((what, op, value));
            match self.next() {
                Some(Token::CloseBracket) => break,
                Some(Token::And) => {}
                Some(Token::Or) => return Err(FilterError::OrInBracket),
                _ => return Err(FilterError::UnclosedBracket { facet }),
            }
        }
        Ok(Expr::Claims { facet, of })
    }
}

fn name_of(what: Claim) -> String {
    match what {
        Claim::Label => "label",
        Claim::Basis => "basis",
        Claim::Confidence => "confidence",
    }
    .to_string()
}

/// What a closure walk already answered, for the life of one compiled filter.
///
/// Keyed by the row's value, because that is what the walk starts from: the
/// pattern and the reach are fixed for a given comparison, so the label is the
/// whole question.
#[derive(Default)]
struct Memo(std::cell::RefCell<std::collections::HashMap<String, bool>>);

fn compare(
    selector: Selector,
    op: Op,
    value: &Value,
    t: &Subject<'_>,
    env: &Env,
    memo: &Memo,
) -> bool {
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
                    Reach::OrNarrower if glob.matches(v) => true,
                    _ => {
                        if let Some(known) = memo.0.borrow().get(v) {
                            return *known;
                        }
                        let reached = env.listened.is_some_and(|l| {
                            l.broader(f.name().as_str(), v)
                                .iter()
                                .any(|up| glob.matches(up))
                        });
                        memo.0.borrow_mut().insert(v.clone(), reached);
                        reached
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
        // Primary and secondaries alike, so `release_type="compilation"`
        // finds an `album/compilation` — #133.
        Field::ReleaseType => r.release_types(),
        Field::ReleaseHeld => vec![r.release_held.written().to_string()],
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
    OpenBracket,
    CloseBracket,
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
            Token::OpenBracket => "[".into(),
            Token::CloseBracket => "]".into(),
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
            '[' => out.push(Token::OpenBracket),
            ']' => out.push(Token::CloseBracket),
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
                    if n.is_whitespace() || "&|!()[]=<>\"".contains(n) {
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

        // **`genre[…]` is a correlated existential over one row's claims**
        // (#93). Read here, before the field lookup, because the bracket
        // changes what the word means: `genre` is a value on the row and
        // `genre[…]` is a question about the assertions behind it.
        if self.peek() == Some(&Token::OpenBracket) {
            // **The second relation the model has** (#79). Same bracket, and
            // deliberately not a facet: a derivation is an edge between
            // recordings, not a concept asserted about one.
            if let Some(works) = match name.as_str() {
                "derived" => Some(false),
                "work_derived" => Some(true),
                _ => None,
            } {
                self.next();
                return self.derivation(works);
            }
            let Some(facet) = crate::model::facet::key_of(&name) else {
                return Err(FilterError::NotAnAxis { name });
            };
            self.next();
            return self.claims(facet);
        }

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
        h.upsert(t.clone());
        let l = crate::fixture::from_holdings(&h);
        let id = crate::model::id::Id::of_audio(&t.held().audio);
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

    /// **A key is a claim on an axis, so the query language already has it**
    /// — #136. A new axis costs one line in `facet::ALL`; this is what that
    /// line buys.
    #[test]
    fn a_recording_is_found_by_the_key_it_is_in() {
        use crate::model::ingest::Confidence;
        use crate::model::key::Key;
        use crate::model::reference::Source;

        let mut s = crate::scan::Holdings::new();
        s.upsert(crate::fixture::Tagged {
            path: "/m/Band/Record/1.mp3".into(),
            artist: Some("Band".into()),
            album: Some("Record".into()),
            title: Some("Track".into()),
            audio_id: Some(crate::audio_id::AudioId { scheme: 1, hash: 7 }),
            ..Default::default()
        });
        let mut log = crate::model::instance::Instance::new(crate::fixture::device());
        log.ingest(100, &s.tags());
        let l = crate::fixture::from_holdings(&s);
        let recording =
            crate::model::rows::Listing::recording_of(&l, crate::model::rows::Listing::rows(&l)[0])
                .expect("a fixture row is a recording");
        // The tag said open key; what reaches the log is the canonical
        // spelling, which is the point of parsing at the boundary.
        log.assign_key(
            110,
            recording,
            Key::parse("4m").expect("open key"),
            Source::TagVariant,
            Confidence::Provisional,
        );
        let l = crate::fixture::from_holdings_and(&s, log.records());
        let id = crate::model::rows::Listing::rows(&l)[0];
        let drawn = crate::model::rows::Listing::listed(&l, id)
            .expect("draws")
            .owned();
        let subject = Subject {
            row: drawn.listed(),
            locator: None,
            recording: Some(recording),
        };
        let asks = |q: &str| Expr::parse(q).expect("parses").compile(Env::default())(&subject);

        assert!(asks(r#"key="F#m""#), "the key it was told, canonically");
        assert!(asks("key?"), "and it has one");
        assert!(!asks(r#"key="Am""#));
        assert!(
            !asks(r#"key="4m""#),
            "the notation does not survive the boundary, which is why it is a value"
        );
    }

    /// **A partly held album is askable** — #134, which is the whole issue:
    /// the library knew every track it held and had no idea whether that was
    /// all of them.
    #[test]
    fn a_partly_held_album_can_be_asked_for() {
        let drawn_of = |s: &crate::scan::Holdings| {
            let l = crate::fixture::from_holdings(s);
            let id = crate::model::rows::Listing::rows(&l)[0];
            crate::model::rows::Listing::listed(&l, id)
                .expect("a fixture row draws")
                .owned()
        };

        // Two tracks of a record the tags say has three.
        let mut short = crate::scan::Holdings::new();
        for (n, title) in [(1u32, "One"), (2, "Two")] {
            short.upsert(crate::fixture::Tagged {
                path: format!("/m/Band/Record/{n}.mp3").into(),
                artist: Some("Band".into()),
                album: Some("Record".into()),
                title: Some(title.into()),
                tracknumber: Some(n),
                track_total: Some(3),
                disc_total: Some(1),
                ..Default::default()
            });
        }
        let drawn = drawn_of(&short);
        let subject = Subject {
            row: drawn.listed(),
            locator: None,
            recording: None,
        };
        let asks = |q: &str| Expr::parse(q).expect("parses").compile(Env::default())(&subject);
        assert!(asks(r#"release_held="partial""#), "two of three is partial");
        assert!(!asks(r#"release_held="complete""#));

        // **The third answer shows itself.** Without a total the library
        // cannot tell short from whole, and saying "complete" would be the one
        // wrong answer that loses the question.
        let mut quiet = crate::scan::Holdings::new();
        quiet.upsert(crate::fixture::Tagged {
            path: "/m/Band/Other/1.mp3".into(),
            artist: Some("Band".into()),
            album: Some("Other".into()),
            title: Some("Alone".into()),
            tracknumber: Some(1),
            ..Default::default()
        });
        assert_eq!(
            drawn_of(&quiet).release_held,
            crate::model::seam::Held::Unknown,
            "nothing said how long the record is"
        );
    }

    /// **`release_type` asks the primary type *or* any secondary** — #133.
    ///
    /// Somebody asking for their compilations means an `album/compilation`
    /// too. A filter that read only the primary would answer "album" for every
    /// one of them and find none.
    #[test]
    fn a_release_is_found_by_its_type_and_by_what_else_it_is() {
        use crate::model::spine::{ReleaseSecondary, ReleaseType};

        let mut single = track();
        single.release_kind = ReleaseType::Single;
        assert!(matches(r#"release_type="single""#, &single));
        assert!(
            !matches(r#"release_type="album""#, &single),
            "a single is not an album"
        );

        // The case a flat enum cannot hold: an album that is also a
        // compilation and a DJ mix, all three true at once.
        let mut mixed = track();
        mixed.release_secondary = [ReleaseSecondary::Compilation, ReleaseSecondary::DjMix]
            .into_iter()
            .collect();
        for asked in ["album", "compilation", "dj-mix"] {
            assert!(
                matches(&format!(r#"release_type="{asked}""#), &mixed),
                "{asked} did not match a release that is one"
            );
        }
        assert!(!matches(r#"release_type="live""#, &mixed));

        // **The default answers, so nothing is invisible.** A file whose tags
        // say nothing about the release is an album, which is what `album`
        // meaning "the default" has to imply.
        assert!(matches(r#"release_type="album""#, &track()));
        assert!(matches(r#"release_type?"#, &track()));
    }

    /// **`derived[…]` is a bounded walk up the derivation graph** — #79 and
    /// `domain-model.md` §5, and the same correlated bracket #93 built for
    /// claims applied to the second relation the model has.
    #[test]
    fn a_derivation_is_asked_about_by_kind_and_by_depth() {
        let t = track();
        // A remix one hop back, and an edit of that remix two hops back.
        let h = Fake {
            derived: vec![("remix".into(), 1), ("edit".into(), 2)],
            ..Default::default()
        };

        assert!(matches_env("derived[]", &t, &h, 0), "derived from anything");
        assert!(matches_env("derived[kind=remix]", &t, &h, 0));
        assert!(!matches_env("derived[kind=dub]", &t, &h, 0));
        assert!(matches_env("derived[depth<=2]", &t, &h, 0));
        assert!(!matches_env("derived[depth>2]", &t, &h, 0));

        // **Correlated, which is what the bracket is for.** There is a remix
        // and there is something two hops back, but the remix is *one* hop —
        // so no single edge is both, and two independent tests would have said
        // yes.
        assert!(
            !matches_env("derived[kind=remix & depth=2]", &t, &h, 0),
            "the bracket lost the correlation"
        );
        assert!(matches_env("derived[kind=edit & depth=2]", &t, &h, 0));
    }

    /// **Nothing derived answers no**, rather than an empty walk reading as a
    /// match — the same rule a missing field follows.
    #[test]
    fn a_recording_derived_from_nothing_matches_nothing() {
        let t = track();
        let h = Fake::default();
        assert!(!matches_env("derived[]", &t, &h, 0));
        assert!(!matches_env("derived[kind=remix]", &t, &h, 0));
    }

    /// **The two traversals do not reach into each other.** Recording
    /// derivations answer "which versions"; work derivations answer "which
    /// arrangements", and a query crossing between them would answer a
    /// question nobody asked.
    #[test]
    fn work_derivations_are_a_separate_traversal() {
        let t = track();
        let h = Fake {
            work_derived: vec![("arrangement".into(), 1)],
            ..Default::default()
        };
        assert!(matches_env("work_derived[kind=arrangement]", &t, &h, 0));
        assert!(
            !matches_env("derived[kind=arrangement]", &t, &h, 0),
            "a work derivation answered a recording question"
        );
    }

    /// A kind is a word, so ordering it means nothing and is refused — the
    /// same rule the claims bracket follows. A depth is a number and ordering
    /// it is the entire point.
    #[test]
    fn a_kind_cannot_be_ordered_and_a_depth_can() {
        assert!(Filter::parse("derived[kind>remix]").is_err());
        assert!(Filter::parse("derived[depth<=2]").is_ok());
        assert!(Filter::parse("derived[depth=x]").is_err());
        assert!(Filter::parse("derived[nonsense=1]").is_err());
        // `|` inside a bracket is refused here for the reason it is refused
        // there: ∃x(P∨Q) distributes, so it says nothing the outer `|` cannot.
        assert!(Filter::parse("derived[kind=remix | kind=edit]").is_err());
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
        held.upsert(instrumental.clone());
        let bare = crate::fixture::from_holdings(&held);
        let copy = crate::model::id::Id::of_audio(&instrumental.held().audio);
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

    /// **The bracket correlates, and that is the whole reason it exists** —
    /// #93.
    ///
    /// The review queue is "Discogs said so, and was not sure". Asked as two
    /// independent existentials it also matches a row where **Discogs was
    /// confirmed and somebody else was unsure**, which is a different set and
    /// not the one anybody wants.
    #[test]
    fn a_bracket_asks_about_one_claim_rather_than_two() {
        let t = track();
        // Discogs was confirmed; the *tag* is the unsure one.
        let crossed = Fake {
            claims: vec![
                ("techno".into(), "discogs".into(), "confirmed".into()),
                ("house".into(), "tag".into(), "provisional".into()),
            ],
            ..Fake::default()
        };
        // And here Discogs itself is unsure, which is the review queue.
        let wanted = Fake {
            claims: vec![("techno".into(), "discogs".into(), "provisional".into())],
            ..Fake::default()
        };

        let review = r#"genre[basis=discogs & confidence!=confirmed]"#;
        assert!(matches_env(review, &t, &wanted, 0));
        assert!(
            !matches_env(review, &t, &crossed, 0),
            "the two halves came from different claims"
        );

        // Each half on its own is true of the crossed row, which is exactly
        // what makes the uncorrelated reading wrong rather than merely loose.
        assert!(matches_env("genre[basis=discogs]", &t, &crossed, 0));
        assert!(matches_env(
            r#"genre[confidence!=confirmed]"#,
            &t,
            &crossed,
            0
        ));
    }

    /// **`|` inside a bracket is refused, and nothing is lost.**
    ///
    /// ∃x(P∨Q) ≡ ∃xP ∨ ∃xQ, so the outside form says the same thing. It is
    /// ∃x(P∧Q) that does not distribute — which is why the bracket exists at
    /// all, and why refusing `|` leaves one way to write each meaning.
    #[test]
    fn or_inside_a_bracket_is_refused_because_it_says_nothing_new() {
        let err = Expr::parse("genre[basis=discogs | basis=tag]").expect_err("should refuse");
        assert!(format!("{err}").contains("same question"), "{err}");

        let t = track();
        let h = Fake {
            claims: vec![("techno".into(), "tag".into(), "provisional".into())],
            ..Fake::default()
        };
        assert!(matches_env(
            "genre[basis=discogs] | genre[basis=tag]",
            &t,
            &h,
            0
        ));
    }

    /// A bracket on something that is not an axis, and a claim nothing
    /// carries, both name what would have worked.
    #[test]
    fn a_bracket_says_what_it_would_have_accepted() {
        let err = Expr::parse("artist[basis=discogs]").expect_err("not an axis");
        assert!(format!("{err}").contains("genre[…]"), "{err}");

        let err = Expr::parse("genre[loudness=high]").expect_err("not a claim");
        for named in ["basis", "confidence", "label"] {
            assert!(format!("{err}").contains(named), "{err}");
        }
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
        /// `(label, basis, confidence)` per claim, as the fold holds them.
        claims: Vec<(String, String, String)>,
        /// `(kind, depth)` per derivation edge, as the bounded climb returns.
        derived: Vec<(String, i64)>,
        work_derived: Vec<(String, i64)>,
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
        fn claims(&self, _: Id<Recording>, _: &str) -> Vec<(String, String, String)> {
            self.claims.clone()
        }
        fn derived_from(&self, _: Id<Recording>) -> Vec<(String, i64)> {
            self.derived.clone()
        }
        fn work_derived_from(&self, _: Id<Recording>) -> Vec<(String, i64)> {
            self.work_derived.clone()
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
            claims: Vec::new(),
            derived: Vec::new(),
            work_derived: Vec::new(),
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

#[cfg(test)]
mod perf {
    use super::*;
    use crate::model::entity::{Device, Hierarchy, Recording};
    use crate::model::id::Id;
    use crate::model::instance::Instance;
    use std::time::Instant;

    /// **What closure costs, measured before somebody discovers it** — #93.
    ///
    /// The issue's worry was that "negation with closure is where a naive
    /// lowering gets slow". There is no lowering to be naive about — a filter
    /// compiles to a closure and runs per row — but the worry was right about
    /// the cost, and measuring found it rather than reasoning about it did.
    ///
    /// **60 ms over 20k rows at depth 12**, against §2's 0.92 ms for the flat
    /// language. Sixty times, for something that runs on every keystroke.
    /// Memoising the walk per compiled filter took it to **7.5 ms**, because
    /// the cost is per *label* and a library has a few hundred distinct genres
    /// over thousands of tracks.
    ///
    /// The bound below is roughly six times the measured figure: loose enough
    /// not to fail on a loaded machine, tight enough that losing the memo
    /// fails here rather than in somebody's hands.
    #[test]
    fn a_closure_pass_over_the_whole_library_is_free() {
        // A deep chain, because depth is what closure pays for.
        let chain: Vec<String> = (0..12).map(|n| format!("Genre{n}")).collect();
        let mut i = Instance::new(Id::<Device>::derived(b"perf"));
        let h = Id::<Hierarchy>::derived(b"browse");
        let subject = crate::model::facet::Described::Recording(Id::<Recording>::derived(b"one"));
        for label in &chain {
            i.assert_concept(
                0,
                subject,
                crate::model::facet::GENRE,
                label,
                crate::model::reference::Source::Manual,
                crate::model::ingest::Confidence::Confirmed,
            );
        }
        for pair in chain.windows(2) {
            i.narrows(0, h, crate::model::facet::GENRE, &pair[0], &pair[1]);
        }
        let cat = i.catalogue();
        let env = Env {
            listened: Some(&cat),
            now: 0,
        };

        // The deepest concept, so every row walks the whole chain.
        let mut t = crate::fixture::Tagged {
            genres: vec![chain.last().unwrap().clone()],
            ..Default::default()
        };
        t.title = Some("A Song".into());
        let drawn = t.drawn();
        let subject = Subject {
            row: drawn.listed(),
            locator: None,
            recording: Some(Id::derived(b"one")),
        };

        const ROWS: usize = 20_000;
        let expr = Expr::parse(r#"genre*="Genre0""#).expect("parses");
        let matches = expr.compile(env);
        let started = Instant::now();
        let mut hits = 0usize;
        for _ in 0..ROWS {
            if matches(&subject) {
                hits += 1;
            }
        }
        let took = started.elapsed();
        assert_eq!(hits, ROWS, "the closure did not reach the root");
        println!(
            "closure: {ROWS} rows, depth {} in {:.2} ms",
            chain.len(),
            took.as_secs_f64() * 1000.0
        );
        assert!(
            took.as_millis() < 50,
            "a whole-library closure pass took {took:?} — it runs on every keystroke"
        );
    }
}
