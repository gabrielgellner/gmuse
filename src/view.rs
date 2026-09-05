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
    /// A named catalog — a saved set, however it decides its membership.
    Catalog(String),
    /// A predicate typed at the line.
    Where(Box<crate::filter::Filter>),
    /// A collection's entries: **a sequence**, so it carries an order and its
    /// duplicates until something widens it.
    Collection(Id<Collection>),
}

/// One term, and how it joins what came before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub op: Op,
    pub what: What,
}

/// A composed view: a left fold over terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stack {
    terms: Vec<Term>,
}

/// What resolving a stack produced, and what it cost.
#[derive(Debug)]
pub struct Resolved {
    pub rows: Rows,
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
        self.terms = vec![Term { op: Op::Or, what }];
    }

    /// Add a term — `:view +jazz`, `&year>=2015`, `-genre="*christmas*"`.
    pub fn push(&mut self, op: Op, what: What) {
        // A stack whose first term narrows has to narrow *something*, and the
        // only honest something is the whole library.
        if self.terms.is_empty() && op != Op::Or {
            self.terms.push(Term {
                op: Op::Or,
                what: What::Everything,
            });
        }
        self.terms.push(Term { op, what });
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
    /// separately: `drop_at` guarded the inversion and `set_catalogs`
    /// reintroduced it, so `:view -x` followed by dropping the exclusion
    /// selected exactly what had been excluded.
    fn normalise(&mut self) {
        if self.terms.first().is_some_and(|t| t.op != Op::Or) {
            self.terms.insert(
                0,
                Term {
                    op: Op::Or,
                    what: What::Everything,
                },
            );
        }
        if self.terms.len() == 1 && self.terms[0].what == What::Everything {
            self.terms.clear();
        }
    }

    /// Replace the catalog terms, keeping whatever was typed.
    ///
    /// **`active_catalogs = ["general", "dance"]` is `general or dance`**, and
    /// this is the translation. Which catalogs are selected stopped being a
    /// property of the catalogs the moment the operator left the noun; view 6
    /// still toggles them, and what it toggles is these terms.
    ///
    /// They go on the front, because a selection sets what you are in and a
    /// predicate narrows it. That is the order the two nouns had when they
    /// were two nouns, and it is the order somebody typing `:view dance
    /// &year>=2015` writes.
    pub fn set_catalogs<I: IntoIterator<Item = String>>(&mut self, names: I) {
        let typed: Vec<Term> = self
            .terms
            .iter()
            .filter(|t| matches!(t.what, What::Where(_)))
            .cloned()
            .collect();

        self.terms = names
            .into_iter()
            .map(|name| Term {
                op: Op::Or,
                what: What::Catalog(name),
            })
            .collect();
        self.terms.extend(typed);
        self.normalise();
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
        catalogs: &crate::catalog::Catalogs,
        env: crate::filter::Env<'_>,
    ) -> Resolved {
        let all: Vec<CopyId> = Listing::rows(librarian);
        if self.terms.is_empty() {
            return Resolved {
                rows: Rows::of_query(recordings(librarian, &all)),
                demoted_at: Vec::new(),
            };
        }

        let mut rows: Option<Rows> = None;
        let mut demoted_at = Vec::new();
        for (at, term) in self.terms.iter().enumerate() {
            let theirs = self.select(&term.what, librarian, catalogs, &all, env);
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
            demoted_at,
        }
    }

    fn select(
        &self,
        what: &What,
        librarian: &dyn Listing,
        catalogs: &crate::catalog::Catalogs,
        all: &[CopyId],
        env: crate::filter::Env<'_>,
    ) -> Rows {
        match what {
            What::Everything => Rows::of_query(recordings(librarian, all)),
            What::Catalog(name) => {
                let held = catalogs.named(name, librarian, all, env);
                Rows::of_query(recordings(librarian, &held))
            }
            What::Where(f) => {
                let matches = f.compile(env);
                let held: Vec<CopyId> = all
                    .iter()
                    .copied()
                    .filter(|id| Listing::subject(librarian, *id).is_some_and(|s| matches(&s)))
                    .collect();
                Rows::of_query(recordings(librarian, &held))
            }
            What::Collection(c) => Rows::of_collection(librarian, *c),
        }
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

fn name_of(what: &What) -> String {
    match what {
        What::Everything => "all".to_string(),
        What::Catalog(n) => n.clone(),
        What::Where(f) => f.source.clone(),
        What::Collection(c) => format!("collection:{c}"),
    }
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
    use crate::filter::Filter;
    use crate::model::entity::Device;
    use crate::model::instance::{Instance, Scanned};
    use crate::model::ord::OrdKey;
    use crate::model::seam::Librarian;

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
    fn library() -> (Librarian, Id<Collection>) {
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
        (Librarian::from_catalogue(&i.catalogue()), dance)
    }

    fn env() -> crate::filter::Env<'static> {
        crate::filter::Env {
            listened: None,
            now: 1_000_000,
        }
    }

    fn where_(src: &str) -> What {
        What::Where(Box::new(Filter::parse(src).expect("parses")))
    }

    fn count(s: &Stack, l: &Librarian) -> usize {
        s.resolve(l, &crate::catalog::Catalogs::default(), env())
            .rows
            .len()
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
        s.set(What::Collection(dance));
        s.push(Op::And, where_(r#"genre="Techno""#));

        let out = s.resolve(&l, &crate::catalog::Catalogs::default(), env());
        assert!(
            out.rows.is_sequence(),
            "the list lost its order to a narrowing"
        );
        assert!(out.demoted_at.is_empty());
    }

    /// **And widening it demotes it, naming the term that did.**
    #[test]
    fn widening_a_collection_says_which_term_cost_the_order() {
        let (l, dance) = library();
        let mut s = Stack::default();
        s.set(What::Collection(dance));
        s.push(Op::Or, where_(r#"genre="Folk""#));

        let out = s.resolve(&l, &crate::catalog::Catalogs::default(), env());
        assert!(!out.rows.is_sequence());
        assert_eq!(out.demoted_at, [1], "nobody was told which term did it");
    }

    /// The line that shows the stack reads like what was typed.
    #[test]
    fn the_stack_reads_back_as_it_was_written() {
        let mut s = Stack::default();
        s.set(What::Catalog("dance".into()));
        s.push(Op::And, where_("year>=2015"));
        s.push(Op::AndNot, where_(r#"genre="*christmas*""#));
        assert_eq!(s.label(), r#"dance &year>=2015 -genre="*christmas*""#);
    }
}
