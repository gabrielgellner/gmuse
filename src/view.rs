//! One verb and three operators — `query-language.md` §2, issue #94.
//!
//! Catalogs union because they are catalogs; `:filter` intersects because it is
//! a filter. The operator was baked into the noun, and that is exactly why #28
//! found a "missing cell": a persistent *narrowing* had nowhere to live, and a
//! `not christmas` catalog would have **widened** the library rather than
//! narrowed it.
//!
//! There is no grid. There is one verb and three operators:
//!
//! ```text
//! :view dance                  set it
//! :view +jazz                  or       — widen
//! :view &year>=2015            and      — narrow
//! :view -genre="*christmas*"   and not  — exclude
//! ```
//!
//! `active_catalogs = ["general", "dance"]` becomes `general or dance`, and
//! #28's missing cell is `-`.
//!
//! ## Where this sits
//!
//! [`crate::model::view`] has the shape algebra — set or sequence, and what
//! combining does to it. This has the *terms* a player composes, which is
//! where it has to live: a term may be a `Filter`, and `filter` reads
//! `model::rows`, so the model cannot name one without the layering going
//! backwards.
//!
//! ## The stack is a left fold
//!
//! So `A and (B or C)` is unreachable from it, and it is the first thing
//! anyone will try. Parentheses live in the written form, which is what
//! `:edit` is for (#99). Saying so here because the limitation is by
//! construction rather than by omission.

#![allow(
    dead_code,
    reason = "the composition is built and tested before the UI is rewired to it (#94). `rebuild_library` still hard-codes catalogs-union-then-filter-intersect, and swapping it over needs a recordings-to-rows step the shelf wants — kept separate so the algebra lands proven rather than mid-rewrite"
)]

use crate::model::entity::Collection;
use crate::model::id::Id;
use crate::model::rows::{CopyId, Listing};
use crate::model::view::{Op, Rows};

/// What one term of a composed view selects.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    /// Everything the library holds. What a view starts from when the first
    /// operator narrows rather than sets.
    Everything,
    // **`Catalog(String)` lived here and is gone** (#119).
    //
    // A catalog claimed five things and a saved view does every one: several
    // active at once is `+`, membership from a rule is a predicate, a rule and
    // a collection together is `&`, switching by name is `saved=`. The only
    // residue was "which are on persists across sessions", which is one
    // remembered source line.
    //
    // #94 dissolved the *operator* — `active_catalogs = ["a", "b"]` became
    // `a or b` — and left the *noun* standing, because deleting it was not on
    // that checklist. The config made it worse than it looked:
    // `[catalogs.jazz] collection = "Jazz"` was two independent strings for
    // one thing, free to drift, which is the failure #90 talked us out of one
    // layer down.
    /// A predicate typed at the line.
    Where(Box<crate::query::filter::Filter>),
    /// A collection's entries: **a sequence**, so it carries an order and its
    /// duplicates until something widens it.
    ///
    /// Written `in="Jazz"`. **By name**, so a stack round-trips through its
    /// own label — which is what a saved view stores.
    Collection(String),
    /// A parenthesised sub-view — written `(in="Jazz" + saved="mornings")`.
    ///
    /// **What `:edit` is for** (#99). The stack is a left fold, so
    /// `A + (B & C)` is unreachable from the line — and it is the first thing
    /// anyone tries. A group makes it one term: droppable as a whole, and the
    /// per-term affordance stands down *inside* it, where the fold is no
    /// longer a list.
    ///
    /// The issue expected this case to make a view **opaque**. It does not
    /// have to: what cannot be rendered as a flat list of droppable terms is
    /// the group's insides, not the view — so the control is withdrawn where
    /// it would lie and kept everywhere it would not.
    Group(Box<Stack>),
    /// A saved view, inlined — written `saved="mornings"`.
    ///
    /// **By name, and resolved at use.** Late binding is what makes a saved
    /// view a living definition: editing the one it references changes what
    /// this means, which is the point. The cost is that a view naming one that
    /// has gone fails when it is used, and it must fail *loudly* — a missing
    /// definition quietly selecting everything, or nothing, is the inference
    /// this project keeps turning into a declaration.
    Saved(String),
}

/// Where a saved view's definition comes from.
///
/// **A trait, not the catalogue**, so this module keeps knowing only what it
/// needs: a name in, a source line out. It also makes the cycle and
/// missing-reference tests readable — they define three views in four lines
/// rather than folding a log to get there.
pub trait Definitions {
    /// The source line somebody saved under this name.
    fn source_of(&self, name: &str) -> Option<String>;
}

/// Nothing is saved — for the paths that have no log to ask.
pub struct NoDefinitions;

impl Definitions for NoDefinitions {
    fn source_of(&self, _name: &str) -> Option<String> {
        None
    }
}

impl Definitions for crate::projection::catalogue::Catalogue {
    fn source_of(&self, name: &str) -> Option<String> {
        self.views
            .get(&Id::<crate::model::entity::View>::of_name(name))
            .map(|v| v.source.clone())
    }
}

/// One term, and how it joins what came before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub op: Op,
    pub what: What,
    /// Comment lines written above this term in the edited document (#99).
    ///
    /// **Carried so the round trip is lossless.** A format that admits
    /// comments and then eats them teaches people not to write any, and the
    /// editor is where somebody explains a query to themselves.
    pub note: Vec<String>,
}

/// A composed view: a left fold over terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stack {
    terms: Vec<Term>,
}

/// Something that went wrong resolving a stack, said out loud.
///
/// **Neither of these can be a silent empty result** (#119). A saved view that
/// names one which has gone, or one that names itself, has to say so — quietly
/// selecting nothing looks exactly like a query that matched nothing, and
/// telling those apart by staring at the screen is the failure this project
/// keeps paying for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A term named a saved view nothing defines. **Reported at use, not at
    /// load**: late binding is what makes a saved view a living definition,
    /// and the price of that is that a reference can go stale between the two.
    Missing(String),
    /// A saved view reaches itself. The names, in the order they were entered,
    /// so the loop can be read rather than deduced.
    Cycle(Vec<String>),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Missing(name) => write!(f, "no saved view {name:?}"),
            Problem::Cycle(names) => write!(f, "saved views loop: {}", names.join(" → ")),
        }
    }
}

/// How deep saved views may nest before it is called a loop.
///
/// A backstop beside the visited-set, not instead of it: the set catches a
/// true cycle, and this catches a chain long enough that resolving it was a
/// mistake whether or not it closes.
const MAX_DEPTH: usize = 16;

/// What resolving a stack produced, and what it cost.
#[derive(Debug)]
pub struct Resolved {
    pub rows: Rows,
    /// Saved views that could not be resolved. Empty in the ordinary case.
    pub problems: Vec<Problem>,
    /// Terms that demoted a sequence to a set, by their position in the stack.
    ///
    /// **Reported rather than swallowed** (#92): `+` has no defensible order
    /// for a hand-made list, so it gives the shape up — and somebody who has
    /// just lost their order should be told which term did it.
    pub demoted_at: Vec<usize>,
}

impl Resolved {
    /// The rows a shelf draws for this view.
    ///
    /// **A view yields recordings and a shelf's membership is copies**
    /// (`query-language.md` §1, #78), so the two are one hop apart and this is
    /// the hop. It belongs here rather than at the call site because the
    /// direction matters: a recording with two copies contributes both rows,
    /// which is what makes an album with a FLAC and an MP3 show the file you
    /// actually hold rather than one of them.
    ///
    /// Order is not carried across. A sequence's order is over recordings, and
    /// a shelf sorts its own rows by artist and album — #97's presentation
    /// question, and not this one.
    pub fn copies(&self, librarian: &dyn Listing) -> std::collections::HashSet<CopyId> {
        let wanted: std::collections::HashSet<_> = self.rows.iter().collect();
        Listing::rows(librarian)
            .into_iter()
            .filter(|id| Listing::recording_of(librarian, *id).is_some_and(|r| wanted.contains(&r)))
            .collect()
    }
}

// **`Fresh` lived here and is gone** (#120).
//
// It overlaid one answer the librarian could not give: a collection written
// this session, because the fold was as old as startup and a catalog resolves
// membership through `Listing` (#94). It worked, and it was a patch — the next
// fact the UI wrote that a reader takes through the seam would have wanted a
// second one, and then there are two producers for one row again.
//
// The loop returns a reason now, `main` refolds, and the fold is simply
// current. What survives from that patch is the better part: `Rows::of_collection`
// and `Stack::resolve` take `&dyn Listing` rather than `&Librarian`, which is
// what the seam was for.

impl Stack {
    /// Replace the stack with one term — `:view dance`.
    pub fn set(&mut self, what: What) {
        self.terms = vec![Term {
            op: Op::Or,
            what,
            note: Vec::new(),
        }];
    }

    /// Add a term — `:view +jazz`, `&year>=2015`, `-genre="*christmas*"`.
    pub fn push(&mut self, op: Op, what: What) {
        // A stack whose first term narrows has to narrow *something*, and the
        // only honest something is the whole library.
        if self.terms.is_empty() && op != Op::Or {
            self.terms.push(Term {
                op: Op::Or,
                what: What::Everything,
                note: Vec::new(),
            });
        }
        self.terms.push(Term {
            op,
            what,
            note: Vec::new(),
        });
    }

    /// Drop one term, so a composition can be taken apart as well as built.
    pub fn drop_at(&mut self, at: usize) {
        if at < self.terms.len() {
            self.terms.remove(at);
        }
        self.normalise();
    }

    /// Put the stack back into a shape that means what it says.
    ///
    /// **Two rules, and both are about the first term.** The fold takes it as
    /// the starting set whatever its operator, so a stack beginning with `-x`
    /// would resolve to *x* — the exact opposite. `Everything` goes on the
    /// front instead. And a lone `Everything` that nothing narrows is the
    /// whole library said the long way, so it goes.
    ///
    /// One function because every rebuild needs it and two of them had it
    /// separately: `drop_at` guarded the inversion and the catalog sync
    /// reintroduced it, so `:view -x` followed by dropping the exclusion
    /// selected exactly what had been excluded.
    fn normalise(&mut self) {
        if self.terms.first().is_some_and(|t| t.op != Op::Or) {
            self.terms.insert(
                0,
                Term {
                    op: Op::Or,
                    what: What::Everything,
                    note: Vec::new(),
                },
            );
        }
        if self.terms.len() == 1 && self.terms[0].what == What::Everything {
            self.terms.clear();
        }
    }

    // **`set_catalogs` lived here and is gone** (#119).
    //
    // It rewrote the front of the stack from the active-catalog list on every
    // rebuild, which is what made a catalog a second kind of term rather than
    // a name for a stack. Its parting gift was a bug: it kept `Where` terms
    // and dropped everything else, so `in=` and `saved=` were silently
    // discarded the moment they existed.
    //
    // What replaced it is that a saved view is applied like anything else —
    // `:view saved="dance"` is a term, and the stack is the only state.

    /// Read a stack back from its own label — the round trip a saved view
    /// depends on, since what is stored is the text.
    ///
    /// **Through the same parser the `:` line uses.** A second parser here
    /// would be a second dialect, and the first operator added to one and not
    /// the other would make a saved view mean something it did not say.
    pub fn parse(source: &str) -> Result<Self, crate::cmd::parse::ParseError> {
        Stack::parse_at(source, 0)
    }

    /// **Nesting is bounded**, for the same reason the saved-view walk is:
    /// text can nest as deep as somebody types, and a parser that recurses on
    /// it hands a stack overflow to a person who typed too many brackets.
    fn parse_at(source: &str, depth: usize) -> Result<Self, crate::cmd::parse::ParseError> {
        use crate::model::view::Op;

        let mut out = Stack::default();
        for (at, piece) in split_terms(source).into_iter().enumerate() {
            // The sign is the stack's, so it comes off before the term is read
            // — a group's insides are bound by the group's own operators.
            let (op, rest) = match piece.split_at_checked(1) {
                Some(("+", tail)) => (Op::Or, tail),
                Some(("&", tail)) => (Op::And, tail),
                Some(("-", tail)) => (Op::AndNot, tail),
                _ => (Op::Or, piece.as_str()),
            };
            let what = match group_body(rest) {
                Some(body) => {
                    if depth >= MAX_DEPTH {
                        return Err(crate::cmd::parse::ParseError::Bad(
                            piece.clone(),
                            "view group, nested too deep",
                        ));
                    }
                    What::Group(Box::new(Stack::parse_at(body, depth + 1)?))
                }
                None => match crate::cmd::parse::view_term(&piece)? {
                    crate::cmd::ViewTerm::Compose(_, what, _) => *what,
                    _ => return Err(crate::cmd::parse::ParseError::Bad(piece, "view term")),
                },
            };
            if at == 0 {
                out.set(what);
                // `set` starts the stack, so a first term that narrows needs
                // `Everything` put in front of it exactly as `push` would.
                if op != Op::Or {
                    out.terms[0].op = op;
                    out.normalise();
                }
            } else {
                out.push(op, what);
            }
        }
        Ok(out)
    }

    /// Take the typed predicates back off, leaving the saved terms.
    ///
    /// What `:filter` with no argument means: it clears what you typed, not
    /// what you had selected. A catalog is a saved set and survives.
    pub fn clear_predicates(&mut self) {
        self.terms.retain(|t| !matches!(t.what, What::Where(_)));
        self.normalise();
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Fold the stack into rows.
    ///
    /// An empty stack is the whole library, which is what "no catalog active
    /// and no filter" has always meant.
    pub fn resolve(
        &self,
        librarian: &dyn Listing,
        defs: &dyn Definitions,
        env: crate::query::filter::Env<'_>,
    ) -> Resolved {
        let all: Vec<CopyId> = Listing::rows(librarian);
        if self.terms.is_empty() {
            return Resolved {
                rows: Rows::of_query(recordings(librarian, &all)),
                problems: Vec::new(),
                demoted_at: Vec::new(),
            };
        }

        let mut rows: Option<Rows> = None;
        let mut demoted_at = Vec::new();
        let mut problems = Vec::new();
        for (at, term) in self.terms.iter().enumerate() {
            let theirs = self.select(
                &term.what,
                librarian,
                defs,
                &all,
                env,
                &mut Vec::new(),
                &mut problems,
            );
            rows = Some(match rows {
                None => theirs,
                Some(mine) => {
                    let out = mine.combine(term.op, &theirs);
                    if out.demoted {
                        demoted_at.push(at);
                    }
                    out.rows
                }
            });
        }
        Resolved {
            rows: rows.unwrap_or_else(|| Rows::of_query(std::iter::empty())),
            problems,
            demoted_at,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn select(
        &self,
        what: &What,
        librarian: &dyn Listing,
        defs: &dyn Definitions,
        all: &[CopyId],
        env: crate::query::filter::Env<'_>,
        entering: &mut Vec<String>,
        problems: &mut Vec<Problem>,
    ) -> Rows {
        match what {
            What::Everything => Rows::of_query(recordings(librarian, all)),
            What::Where(f) => {
                let matches = f.compile(env);
                let held: Vec<CopyId> = all
                    .iter()
                    .copied()
                    .filter(|id| Listing::subject(librarian, *id).is_some_and(|s| matches(&s)))
                    .collect();
                Rows::of_query(recordings(librarian, &held))
            }
            What::Collection(name) => {
                Rows::of_collection(librarian, Id::<Collection>::of_name(name))
            }
            What::Saved(name) => {
                // **The loop is caught before it is taken**, and reported with
                // the path that closes it — a stack overflow says nothing
                // about which two definitions disagree.
                if entering.iter().any(|seen| seen == name) {
                    let mut loop_ = entering.clone();
                    loop_.push(name.clone());
                    problems.push(Problem::Cycle(loop_));
                    return Rows::of_query(std::iter::empty());
                }
                if entering.len() >= MAX_DEPTH {
                    problems.push(Problem::Cycle(entering.clone()));
                    return Rows::of_query(std::iter::empty());
                }
                let Some(source) = defs.source_of(name) else {
                    problems.push(Problem::Missing(name.clone()));
                    return Rows::of_query(std::iter::empty());
                };
                let Ok(inner) = Stack::parse(&source) else {
                    // A definition that no longer parses is as broken as a
                    // missing one, and for the reader it is the same problem.
                    problems.push(Problem::Missing(name.clone()));
                    return Rows::of_query(std::iter::empty());
                };

                entering.push(name.clone());
                let rows = self.fold(&inner, librarian, defs, all, env, entering, problems);
                entering.pop();
                rows
            }
            // **A group is folded, not flattened.** Flattening it here would
            // put its terms back into the outer left fold, which is the exact
            // grouping it exists to escape.
            What::Group(inner) => self.fold(inner, librarian, defs, all, env, entering, problems),
        }
    }

    /// Fold a stack's terms into rows, ignoring demotion.
    ///
    /// Shared by a saved view and a group, which are the two ways one stack
    /// resolves inside another. `resolve` keeps its own copy because it is the
    /// only one that reports which term cost the order — inside a nested view
    /// there is no term number to report it against.
    #[allow(clippy::too_many_arguments)]
    fn fold(
        &self,
        stack: &Stack,
        librarian: &dyn Listing,
        defs: &dyn Definitions,
        all: &[CopyId],
        env: crate::query::filter::Env<'_>,
        entering: &mut Vec<String>,
        problems: &mut Vec<Problem>,
    ) -> Rows {
        let mut rows: Option<Rows> = None;
        for term in &stack.terms {
            let theirs = self.select(&term.what, librarian, defs, all, env, entering, problems);
            rows = Some(match rows {
                None => theirs,
                Some(mine) => mine.combine(term.op, &theirs).rows,
            });
        }
        rows.unwrap_or_else(|| Rows::of_query(recordings(librarian, all)))
    }

    /// The stack as somebody typed it, for the line that shows it.
    pub fn label(&self) -> String {
        self.terms
            .iter()
            .enumerate()
            .map(|(at, t)| {
                let sign = match (at, t.op) {
                    (0, _) => "",
                    (_, Op::Or) => "+",
                    (_, Op::And) => "&",
                    (_, Op::AndNot) => "-",
                };
                format!("{sign}{}", name_of(&t.what))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A view written out for `$EDITOR`, and read back — issue #99.
///
/// **The same grammar as the line, with two things that are inert on one.**
/// A newline is whitespace, so a long composition can be laid out one term to
/// a line; and `#` starts a comment, which the line would never carry because
/// there is nowhere to put it. Neither is a second syntax: every term is the
/// text `:view` would take, and the operators are the stack's `+ & -`.
///
/// That the escape hatch is the *same text* is what makes the three tiers one
/// language rather than three — `query-language.md` §11. A structured save
/// format would make the editor a translation.
impl Stack {
    /// The composed view, one term to a line, with its comments.
    ///
    /// Deliberately no generated header. Anything this wrote as a preamble
    /// would come back as a comment on the first term and be written again,
    /// so the file would grow its own instructions every time it was opened.
    /// What the format is belongs in the guide; what the file holds is the
    /// query.
    pub fn document(&self) -> String {
        let mut out = String::new();
        for (at, term) in self.terms.iter().enumerate() {
            for line in &term.note {
                out.push_str(line);
                out.push('\n');
            }
            let sign = match (at, term.op) {
                (0, _) => "",
                (_, Op::Or) => "+ ",
                (_, Op::And) => "& ",
                (_, Op::AndNot) => "- ",
            };
            out.push_str(sign);
            out.push_str(&name_of(&term.what));
            out.push('\n');
        }
        out
    }

    /// Read a document back, naming the line a bad term is on.
    ///
    /// **The line number is the point of the whole function.** A parse error
    /// against a fifteen-line file that does not say where is a worse message
    /// than the one-line form gives, and the editor exists for the queries too
    /// long to read at a glance.
    pub fn parse_document(text: &str) -> Result<Self, DocumentError> {
        let mut source = String::new();
        // Where each term *starts*, by the byte offset of its first character
        // — filled in as the comment-stripped source is built, so a term
        // spanning three lines is reported against the line it opened on.
        let mut lines: Vec<(usize, usize)> = Vec::new();
        // Where a run of comment lines started, and what it said.
        let mut notes: Vec<(usize, Vec<String>)> = Vec::new();
        let mut pending: Vec<String> = Vec::new();

        for (at, raw) in text.lines().enumerate() {
            let (code, comment) = split_comment(raw);
            if let Some(c) = comment
                && code.trim().is_empty()
            {
                pending.push(c.to_string());
                continue;
            }
            if code.trim().is_empty() {
                continue;
            }
            lines.push((source.len(), at + 1));
            if !pending.is_empty() {
                notes.push((source.len(), std::mem::take(&mut pending)));
            }
            source.push_str(code);
            source.push(' ');
        }
        // A trailing comment with no term under it has nothing to hang on, and
        // silently eating it would be the losing half of "text all the way
        // down". It joins the last term instead.
        if !pending.is_empty()
            && let Some((at, _)) = lines.last()
        {
            notes.push((*at, pending));
        }

        let mut out = Stack::parse(&source).map_err(|error| DocumentError {
            line: line_of(&source, &lines, &error),
            error,
        })?;
        attach_notes(&mut out, &source, &notes);
        Ok(out)
    }
}

/// A document that did not parse, and the line it went wrong on.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentError {
    /// 1-based, so it matches what the editor shows.
    pub line: usize,
    pub error: crate::cmd::parse::ParseError,
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.error)
    }
}

impl std::error::Error for DocumentError {}

/// Split a line into its code and its comment.
///
/// **Quote-aware**, because `genre="#1 hits"` is a genre and not a comment —
/// the one case that makes a naive `find('#')` wrong.
fn split_comment(line: &str) -> (&str, Option<&str>) {
    let mut quoted = false;
    for (at, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return (&line[..at], Some(&line[at..])),
            _ => {}
        }
    }
    (line, None)
}

/// Which line the term holding this error opened on.
///
/// The stack parser reports the offending *piece*, so the piece is found in
/// the joined source and the line whose span covers it is the answer. A error
/// that names no piece falls back to the first line, which is the honest
/// answer for "the document as a whole".
fn line_of(source: &str, lines: &[(usize, usize)], error: &crate::cmd::parse::ParseError) -> usize {
    let needle = piece_of(error);
    let at = needle
        .and_then(|n| source.find(n.trim()))
        .unwrap_or(usize::MAX);
    lines
        .iter()
        .take_while(|(start, _)| *start <= at)
        .last()
        .or_else(|| lines.first())
        .map_or(1, |(_, line)| *line)
}

/// The text a parse error was raised against, when it names one.
fn piece_of(error: &crate::cmd::parse::ParseError) -> Option<&str> {
    use crate::cmd::parse::ParseError;
    match error {
        ParseError::Bad(what, _) | ParseError::Bare(what) => Some(what),
        _ => None,
    }
}

/// Put the comment lines back on the terms they were written above.
///
/// Matched by where each term started in the joined source, which is the same
/// thing `split_terms` cut on — so a term that moved because `normalise` put
/// `all` in front of it keeps its own comment rather than the next one's.
fn attach_notes(stack: &mut Stack, source: &str, notes: &[(usize, Vec<String>)]) {
    let starts: Vec<usize> = split_terms(source)
        .iter()
        .scan(0usize, |from, piece| {
            let at = source[*from..].find(piece.trim()).map(|i| *from + i)?;
            *from = at + piece.trim().len();
            Some(at)
        })
        .collect();
    for (at, body) in notes {
        let Some(which) = starts.iter().position(|s| s >= at) else {
            continue;
        };
        // `normalise` may have inserted `all` at the front, so the stack can
        // hold one more term than the source had pieces.
        let offset = stack.terms.len().saturating_sub(starts.len());
        if let Some(term) = stack.terms.get_mut(which + offset) {
            term.note.clone_from(body);
        }
    }
}

/// One term as it would be typed — the round trip a saved view rests on.
pub fn written(what: &What) -> String {
    name_of(what)
}

fn name_of(what: &What) -> String {
    match what {
        What::Everything => "all".to_string(),
        What::Where(f) => f.source.clone(),
        What::Collection(name) => format!("in={name:?}"),
        What::Saved(name) => format!("saved={name:?}"),
        What::Group(inner) => format!("({})", inner.label()),
    }
}

/// Split a stack's label back into its terms.
///
/// The sign starts a term, and a quoted value may hold one — `-genre="a-b"`
/// splits after the quote, not inside it. **Nor inside parentheses or
/// brackets**: a group is one term at this level, and so is #93's qualifier
/// bracket. The operators binding their insides are theirs, not the stack's.
///
/// The bracket was the live bug — `genre[basis=discogs & confidence!=confirmed]`
/// is one term with a conjunction inside it, and splitting on that `&` cut a
/// filter in half. It only showed on the way *back*: typed at the line the
/// term never passes through here, so a saved view of it broke and a `:edit`
/// of it would have.
fn split_terms(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut depth = 0usize;
    for c in source.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if !quoted && depth == 0 && matches!(c, '+' | '&' | '-') && !current.trim().is_empty() {
            out.push(current.trim().to_string());
            current.clear();
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// The body of a piece that is one balanced parenthesised group, if it is.
///
/// **Balanced end to end**, so `(a) & (b)` is not read as a group whose body
/// is `a) & (b` — the check is not "starts with `(` and ends with `)`".
fn group_body(piece: &str) -> Option<&str> {
    let piece = piece.trim();
    let inner = piece.strip_prefix('(')?.strip_suffix(')')?;
    let (mut depth, mut quoted) = (0usize, false);
    for c in inner.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if quoted {
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
            }
            _ => {}
        }
    }
    (depth == 0).then_some(inner)
}

/// The recordings behind a set of rows.
///
/// A view yields recordings — `query-language.md` §1 — and a row names a copy,
/// so the two are one hop apart. Two copies of one performance are one
/// recording in a view, which is what makes `+` a set union rather than a
/// count.
fn recordings(
    librarian: &dyn Listing,
    rows: &[CopyId],
) -> Vec<Id<crate::model::entity::Recording>> {
    rows.iter()
        .filter_map(|id| Listing::recording_of(librarian, *id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A tree-shaped query round-trips through the document exactly** —
    /// #99's third requirement, and what makes `:edit` a bigger window rather
    /// than a translation.
    #[test]
    fn a_document_round_trips_including_its_comments() {
        let written = concat!(
            "# the ones I actually reach for\n",
            "in=\"Dance\"\n",
            "# but not the seasonal ones\n",
            "- genre=\"*christmas*\"\n",
            "& year>=2015\n",
        );
        let stack = Stack::parse_document(written).expect("it parses");
        assert_eq!(stack.document(), written, "the document did not survive");
        assert_eq!(stack.terms().len(), 3);
        assert_eq!(
            stack.terms()[1].note,
            ["# but not the seasonal ones"],
            "a comment landed on the wrong term"
        );
    }

    /// **A newline is whitespace, not a term separator.** One grammar: the
    /// sign is what starts a term, on a line or on the `:` line.
    #[test]
    fn a_term_may_be_laid_out_over_several_lines() {
        let stack = Stack::parse_document("in=\"Dance\"\n  & year>=2015\n").expect("parses");
        assert_eq!(stack.terms().len(), 2);
        assert_eq!(
            stack.label(),
            Stack::parse("in=\"Dance\" &year>=2015").unwrap().label()
        );
    }

    /// **A bad term is reported against its line**, which is the whole reason
    /// the editor gets a document rather than a line.
    #[test]
    fn a_parse_error_names_the_line_it_is_on() {
        let bad = "in=\"Dance\"\n# a note\n& year>=2015\n+ jazz\n";
        let e = Stack::parse_document(bad).expect_err("`jazz` is a bare word");
        assert_eq!(e.line, 4, "{e}");
        assert!(e.to_string().starts_with("line 4:"), "{e}");
    }

    /// **A comment cannot be a `#` inside a value.** `genre="#1 hits"` is a
    /// genre, and the naive split would have cut it in half.
    #[test]
    fn a_hash_inside_quotes_is_not_a_comment() {
        let stack = Stack::parse_document("genre=\"#1 hits\"\n").expect("parses");
        assert_eq!(stack.terms().len(), 1);
        assert_eq!(stack.document(), "genre=\"#1 hits\"\n");
    }

    /// **`A + (B & C)` is what the editor is for** — unreachable from the left
    /// fold, and the first thing anyone tries.
    #[test]
    fn a_group_is_one_droppable_term_and_writes_itself_back() {
        let stack = Stack::parse_document("in=\"Dance\"\n+ (saved=\"mornings\" & year>=2015)\n")
            .expect("parses");
        assert_eq!(stack.terms().len(), 2, "the group is one term");
        assert!(matches!(stack.terms()[1].what, What::Group(_)));
        assert_eq!(
            stack.document(),
            "in=\"Dance\"\n+ (saved=\"mornings\" &year>=2015)\n"
        );
    }

    /// **The grouping changes the answer**, which is the whole reason a group
    /// exists rather than being flattened back into the fold.
    ///
    /// `Folk + Techno & artist="Bodzin"` folds left: everything folk or
    /// techno, then narrowed to Bodzin — the two techno tracks.
    /// `Folk + (Techno & artist="Bodzin")` is folk *plus* Bodzin's techno —
    /// four. Same terms, same operators, different tree.
    #[test]
    fn a_group_is_not_the_left_fold_of_its_terms() {
        let (l, _) = library();

        let flat = Stack::parse(r#"genre="Folk" +genre="Techno" &artist="Bodzin""#).unwrap();
        assert_eq!(count(&flat, &l), 2);

        let grouped = Stack::parse(r#"genre="Folk" +(genre="Techno" &artist="Bodzin")"#).unwrap();
        assert_eq!(
            count(&grouped, &l),
            4,
            "the group was flattened back into the fold"
        );
    }

    /// **Every shape the language can write survives the round trip**, both
    /// through the label a saved view stores and through the document `:edit`
    /// hands to the editor.
    ///
    /// The bracket is the one that was broken. `genre[basis=discogs &
    /// confidence!=confirmed]` is one term holding a conjunction, and the term
    /// splitter cut it on that `&` — invisible from the line, where a term
    /// never passes through the splitter, and fatal on the way back.
    #[test]
    fn every_written_shape_parses_back_to_itself() {
        for src in [
            r#"in="General""#,
            r#"all -in="Christmas""#,
            r#"in="Dance" +(saved="mornings" &year>=2015)"#,
            r#"genre="Drum & Bass""#,
            r#"any(genre*)="electronic" &rating>=4"#,
            r#"genre[basis=discogs & confidence!=confirmed]"#,
            r#"(genre="folk"|genre="country") &year>2000"#,
        ] {
            let stack = Stack::parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(stack.label(), src, "the label is not what was written");

            let doc = stack.document();
            let back = Stack::parse_document(&doc).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(back.document(), doc, "the document did not survive");
            assert_eq!(back.label(), src, "the document lost what the label had");
        }
    }

    /// **A stack that begins with an exclusion parses back.** `normalise` puts
    /// `all` in front of it, and `all` was a bare word — so every such view
    /// failed to round-trip, and a saved one reported as missing.
    #[test]
    fn a_view_that_starts_by_excluding_survives_being_saved() {
        let mut stack = Stack::default();
        stack.push(Op::AndNot, What::Collection("Christmas".into()));
        let back = Stack::parse(&stack.label()).expect("`all` is a keyword");
        assert_eq!(back.label(), stack.label());
    }

    use crate::model::entity::Device;
    use crate::model::instance::{Instance, Scanned};
    use crate::model::ord::OrdKey;
    use crate::model::seam::Librarian;
    use crate::query::filter::Filter;

    fn scanned(title: &str, artist: &str, genre: &str, hash: u128) -> Scanned {
        Scanned {
            audio: crate::audio_id::AudioId { scheme: 1, hash },
            title: title.into(),
            artist: artist.into(),
            album: Some("An Album".into()),
            genres: vec![genre.to_string()],
            track: Some(1),
            ..Default::default()
        }
    }

    /// A library of four, and a `Dance` collection holding two of them.
    fn library() -> (Librarian, String) {
        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        i.ingest(
            100,
            &[
                scanned("Techno One", "Bodzin", "Techno", 1),
                scanned("Techno Two", "Bodzin", "Techno", 2),
                scanned("A Folk Song", "Ferrell", "Folk", 3),
                scanned("Another Folk", "Ferrell", "Folk", 4),
            ],
        );
        let dance = Id::<Collection>::of_name("Dance");
        let recs: Vec<_> = i.catalogue().recordings.keys().copied().collect();
        let mut keys: Vec<OrdKey> = Vec::new();
        for r in recs.iter().take(2) {
            let k = OrdKey::between(keys.last(), None);
            i.add_to_collection(200, dance, *r, k.clone());
            keys.push(k);
        }
        (
            Librarian::from_catalogue(&i.catalogue()),
            "Dance".to_string(),
        )
    }

    fn env() -> crate::query::filter::Env<'static> {
        crate::query::filter::Env {
            listened: None,
            named: None,
            now: 1_000_000,
        }
    }

    fn where_(src: &str) -> What {
        What::Where(Box::new(Filter::parse(src).expect("parses")))
    }

    fn count(s: &Stack, l: &Librarian) -> usize {
        s.resolve(l, &NoDefinitions, env()).rows.len()
    }

    /// An empty stack is the whole library, which is what "no catalog and no
    /// filter" has always meant.
    #[test]
    fn nothing_composed_is_everything() {
        let (l, _) = library();
        assert_eq!(count(&Stack::default(), &l), 4);
    }

    /// **The three operators, on one verb.**
    #[test]
    fn one_verb_narrows_widens_and_excludes() {
        let (l, _) = library();

        let mut narrow = Stack::default();
        narrow.push(Op::And, where_(r#"genre="Techno""#));
        assert_eq!(count(&narrow, &l), 2, "& did not narrow");

        let mut exclude = Stack::default();
        exclude.push(Op::AndNot, where_(r#"genre="Techno""#));
        assert_eq!(count(&exclude, &l), 2, "- did not exclude");

        let mut widen = Stack::default();
        widen.set(where_(r#"genre="Techno""#));
        widen.push(Op::Or, where_(r#"genre="Folk""#));
        assert_eq!(count(&widen, &l), 4, "+ did not widen");
    }

    /// **#28's missing cell.** A persistent narrowing had nowhere to live, and
    /// a `not christmas` catalog would have widened the library.
    #[test]
    fn a_narrowing_can_be_persistent_and_composed_with() {
        let (l, _) = library();
        let mut s = Stack::default();
        s.set(where_(r#"artist="Bodzin""#));
        s.push(Op::AndNot, where_(r#"title="*Two*""#));
        assert_eq!(count(&s, &l), 1, "the exclusion did not stick");
    }

    /// A stack that begins by narrowing narrows the whole library, because the
    /// only honest thing to narrow is everything.
    #[test]
    fn a_leading_narrowing_narrows_the_library() {
        let (l, _) = library();
        let mut s = Stack::default();
        s.push(Op::And, where_(r#"genre="Folk""#));
        assert_eq!(s.terms().len(), 2, "nothing was there to narrow");
        assert_eq!(s.terms()[0].what, What::Everything);
        assert_eq!(count(&s, &l), 2);
    }

    /// **Each term is droppable**, so a composition comes apart as well as
    /// together.
    #[test]
    fn dropping_a_term_leaves_the_rest_composed() {
        let (l, _) = library();
        let mut s = Stack::default();
        s.set(where_(r#"artist="Bodzin""#));
        s.push(Op::AndNot, where_(r#"title="*Two*""#));
        assert_eq!(count(&s, &l), 1);

        s.drop_at(1);
        assert_eq!(
            count(&s, &l),
            2,
            "dropping the exclusion did not restore it"
        );
    }

    /// **And dropping the *first* term must not invert what is left.**
    ///
    /// A stack beginning with `-x` has nothing to exclude from, and the fold
    /// takes its first term as the starting set whatever its operator — so
    /// `[-x]` would resolve to *x*, the exact opposite of what it says. The
    /// missing `all` is put back on the front instead.
    ///
    /// The first draft of this dropped a leading term before an `&` and could
    /// not tell: `all & x` and `x` are the same set, so the guard looked
    /// unnecessary. `-` is where it shows.
    #[test]
    fn dropping_the_first_term_does_not_invert_what_is_left() {
        let (l, _) = library();
        let mut s = Stack::default();
        s.set(where_(r#"artist="Bodzin""#));
        s.push(Op::AndNot, where_(r#"title="*Two*""#));
        s.drop_at(0);

        assert_eq!(
            count(&s, &l),
            3,
            "a dangling exclusion selected what it was meant to exclude"
        );
    }

    /// **A collection is a sequence, and narrowing keeps it one** (#92).
    #[test]
    fn narrowing_a_collection_keeps_its_order() {
        let (l, dance) = library();
        let mut s = Stack::default();
        s.set(What::Collection(dance.clone()));
        s.push(Op::And, where_(r#"genre="Techno""#));

        let out = s.resolve(&l, &NoDefinitions, env());
        assert!(
            out.rows.is_sequence(),
            "the list lost its order to a narrowing"
        );
        assert!(out.demoted_at.is_empty());
    }

    /// Definitions a test writes by hand, so a cycle is four lines rather
    /// than a folded log.
    struct Defs(Vec<(String, String)>);

    impl Defs {
        fn of(pairs: &[(&str, &str)]) -> Self {
            Defs(
                pairs
                    .iter()
                    .map(|(n, s)| ((*n).to_string(), (*s).to_string()))
                    .collect(),
            )
        }
    }

    impl Definitions for Defs {
        fn source_of(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| s.clone())
        }
    }

    /// **A stack round-trips through its own label**, which is what lets a
    /// saved view store text rather than a parsed structure (#119).
    ///
    /// If this ever fails, a saved view means something other than what was
    /// typed — and it would fail silently, on reload, long after the edit.
    #[test]
    fn a_stack_round_trips_through_its_label() {
        for source in [
            r#"in="Dance""#,
            r#"in="Dance" &genre="Techno""#,
            r#"saved="mornings" -genre="Folk""#,
            r#"genre="Techno" +in="Dance" -year<2000"#,
        ] {
            let parsed = Stack::parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
            assert_eq!(parsed.label(), source, "did not round-trip");
        }
    }

    /// **A saved view is inlined at use**, so editing what it names changes
    /// what it means — the whole point of late binding.
    #[test]
    fn a_saved_view_resolves_to_what_it_names() {
        let (l, dance) = library();
        let source = format!("in={dance:?}");
        let defs = Defs::of(&[("crate", &source)]);

        let mut s = Stack::default();
        s.set(What::Saved("crate".into()));
        let out = s.resolve(&l, &defs, env());
        assert!(out.problems.is_empty(), "{:?}", out.problems);
        assert_eq!(out.rows.len(), 2, "the definition was not inlined");
    }

    /// **A missing reference is reported, not an empty result.**
    ///
    /// Late binding means a reference can go stale between the save and the
    /// use, and an empty library looks exactly like a query that matched
    /// nothing. Only saying so tells them apart.
    #[test]
    fn a_view_naming_one_that_is_gone_says_so() {
        let (l, _) = library();
        let mut s = Stack::default();
        s.set(What::Saved("vanished".into()));

        let out = s.resolve(&l, &NoDefinitions, env());
        assert_eq!(out.problems, [Problem::Missing("vanished".into())]);
        assert_eq!(out.rows.len(), 0);
    }

    /// **A loop is caught with the path that closes it**, not with a stack
    /// overflow — which says nothing about which two definitions disagree.
    #[test]
    fn saved_views_that_reference_each_other_are_a_named_error() {
        let (l, _) = library();
        let defs = Defs::of(&[
            ("a", r#"saved="b""#),
            ("b", r#"saved="c""#),
            ("c", r#"saved="a""#),
        ]);

        let mut s = Stack::default();
        s.set(What::Saved("a".into()));
        let out = s.resolve(&l, &defs, env());
        assert_eq!(
            out.problems,
            [Problem::Cycle(vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "a".into()
            ])],
            "the loop was not named"
        );
    }

    /// One that names itself directly, which is the accident somebody
    /// actually makes.
    #[test]
    fn a_view_that_names_itself_is_a_loop_of_one() {
        let (l, _) = library();
        let defs = Defs::of(&[("me", r#"saved="me""#)]);
        let mut s = Stack::default();
        s.set(What::Saved("me".into()));
        let out = s.resolve(&l, &defs, env());
        assert_eq!(
            out.problems,
            [Problem::Cycle(vec!["me".into(), "me".into()])]
        );
    }

    /// **And widening it demotes it, naming the term that did.**
    #[test]
    fn widening_a_collection_says_which_term_cost_the_order() {
        let (l, dance) = library();
        let mut s = Stack::default();
        s.set(What::Collection(dance.clone()));
        s.push(Op::Or, where_(r#"genre="Folk""#));

        let out = s.resolve(&l, &NoDefinitions, env());
        assert!(!out.rows.is_sequence());
        assert_eq!(out.demoted_at, [1], "nobody was told which term did it");
    }

    /// The line that shows the stack reads like what was typed.
    #[test]
    fn the_stack_reads_back_as_it_was_written() {
        let mut s = Stack::default();
        s.set(What::Saved("dance".into()));
        s.push(Op::And, where_("year>=2015"));
        s.push(Op::AndNot, where_(r#"genre="*christmas*""#));
        assert_eq!(
            s.label(),
            r#"saved="dance" &year>=2015 -genre="*christmas*""#
        );
    }
}
