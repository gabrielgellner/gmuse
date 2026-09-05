//! What a view holds: a set or a sequence — `query-language.md` §1–§2.
//!
//! A view yields recordings and comes in two shapes, and #92 is the issue that
//! says the distinction has to be load-bearing rather than decorative:
//!
//! - a **set** — membership from a predicate, derived, unordered until a sort
//!   is applied, and each recording in it once
//! - a **sequence** — membership *and* order stored as facts, and it **admits
//!   duplicates**
//!
//! Everything the player calls something else is one of those two. A playlist
//! and the queue are sequences; a smart playlist, a catalog and `:filter` are
//! sets, differing only in which operator combines them with what is already
//! active.
//!
//! ## Why the distinction cannot be dropped
//!
//! A predicate cannot express multiplicity, and playing one recording twice in
//! a queue is ordinary rather than exotic. So "the queue is just a view" is
//! false unless a view can be a sequence — which is what an earlier draft of
//! the design lost and #92 exists to keep.
//!
//! ## What it decides about combining
//!
//! `&` and `-` are safe on a sequence: the result is a **subsequence of the
//! left operand**, so its order and its duplicates survive untouched.
//!
//! `+` has no defensible answer. The order of `crateA + crateB` is not
//! concatenation, not interleaving and not a re-sort, because a stored
//! position is hand-assigned and means nothing beside another list's. So
//! combining a sequence with `+` **demotes it to a set** — and [`Combined`]
//! carries that fact out rather than leaving the interface to silently pick
//! one of the three.

#![allow(
    dead_code,
    reason = "the shape is decided before the language that composes it (#92). #93 is the query surface and #94 the composition operator; both are written against this rather than choosing a shape of their own"
)]

use std::collections::BTreeSet;

use crate::model::entity::{Collection, Recording};
use crate::model::id::Id;

/// A view's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rows {
    /// Derived from a predicate: no order of its own, and no duplicates.
    Set(BTreeSet<Id<Recording>>),
    /// Read from a collection's entries: ordered, and duplicates are entries.
    Sequence(Vec<Id<Recording>>),
}

/// How two views combine — `query-language.md` §2's three operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `+` — widen. **Sets only**; a sequence demotes.
    Or,
    /// `&` — narrow. Safe on a sequence.
    And,
    /// `-` — exclude, which is #28's missing cell. Safe on a sequence.
    AndNot,
}

/// The result of combining, and whether the shape survived it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combined {
    pub rows: Rows,
    /// **A sequence was widened, so it is a set now.**
    ///
    /// Reported rather than swallowed. The interface is meant to say this out
    /// loud: somebody who combined a hand-ordered list with something else has
    /// lost their order, and finding out by looking is worse than being told.
    pub demoted: bool,
}

impl Rows {
    /// A collection's entries, in order and with their duplicates.
    ///
    /// **Through the seam, not through one implementation of it** (#118), so
    /// the player can overlay the entries it has written this session — see
    /// `view::Fresh`.
    pub fn of_collection(
        rows: &dyn crate::model::rows::Listing,
        collection: Id<Collection>,
    ) -> Self {
        Rows::Sequence(rows.entries_of(collection))
    }

    /// Whatever a predicate matched.
    pub fn of_query(matched: impl IntoIterator<Item = Id<Recording>>) -> Self {
        Rows::Set(matched.into_iter().collect())
    }

    /// Is this ordered, and does it admit duplicates?
    pub fn is_sequence(&self) -> bool {
        matches!(self, Rows::Sequence(_))
    }

    /// The recordings, in whatever order this shape has.
    pub fn iter(&self) -> Box<dyn Iterator<Item = Id<Recording>> + '_> {
        match self {
            Rows::Set(s) => Box::new(s.iter().copied()),
            Rows::Sequence(v) => Box::new(v.iter().copied()),
        }
    }

    /// How many rows, counting a duplicate twice where the shape has them.
    pub fn len(&self) -> usize {
        match self {
            Rows::Set(s) => s.len(),
            Rows::Sequence(v) => v.len(),
        }
    }

    /// Combine with another view.
    ///
    /// Narrowing keeps the left operand's shape, because the result is a
    /// subsequence of it. Widening a sequence cannot, and says so.
    pub fn combine(self, op: Op, other: &Rows) -> Combined {
        let theirs: BTreeSet<Id<Recording>> = other.iter().collect();
        match (op, self) {
            // **The one case with no answer.** Concatenation, interleaving and
            // a re-sort are all defensible and none is right, so the shape is
            // given up rather than one of them being chosen quietly.
            (Op::Or, Rows::Sequence(mine)) => Combined {
                rows: Rows::Set(mine.into_iter().chain(theirs).collect()),
                demoted: true,
            },
            (Op::Or, Rows::Set(mine)) => Combined {
                rows: Rows::Set(mine.union(&theirs).copied().collect()),
                demoted: false,
            },
            (Op::And, Rows::Sequence(mut mine)) => {
                mine.retain(|r| theirs.contains(r));
                Combined {
                    rows: Rows::Sequence(mine),
                    demoted: false,
                }
            }
            (Op::AndNot, Rows::Sequence(mut mine)) => {
                mine.retain(|r| !theirs.contains(r));
                Combined {
                    rows: Rows::Sequence(mine),
                    demoted: false,
                }
            }
            (Op::And, Rows::Set(mine)) => Combined {
                rows: Rows::Set(mine.intersection(&theirs).copied().collect()),
                demoted: false,
            },
            (Op::AndNot, Rows::Set(mine)) => Combined {
                rows: Rows::Set(mine.difference(&theirs).copied().collect()),
                demoted: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Device;
    use crate::model::instance::Instance;
    use crate::model::ord::OrdKey;
    use crate::model::seam::Librarian;

    fn song(n: u8) -> Id<Recording> {
        Id::<Recording>::derived(&[n])
    }

    fn set(of: &[u8]) -> Rows {
        Rows::of_query(of.iter().map(|n| song(*n)))
    }

    /// A collection holding exactly these, in this order.
    fn sequence(of: &[u8]) -> Rows {
        let list = Id::<Collection>::derived(b"a list");
        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        let mut keys: Vec<OrdKey> = Vec::new();
        for n in of {
            let k = OrdKey::between(keys.last(), None);
            i.add_to_collection(100, list, song(*n), k.clone());
            keys.push(k);
        }
        Rows::of_collection(&Librarian::from_catalogue(&i.catalogue()), list)
    }

    /// **A sequence keeps its order and its duplicates**, which is the whole
    /// reason the two shapes exist.
    #[test]
    fn a_sequence_is_ordered_and_admits_duplicates() {
        let queue = sequence(&[3, 1, 3]);
        assert!(queue.is_sequence());
        assert_eq!(
            queue.iter().collect::<Vec<_>>(),
            [song(3), song(1), song(3)]
        );
        assert_eq!(queue.len(), 3, "the duplicate was counted once");
    }

    /// And a set has neither, which is what a predicate can express.
    #[test]
    fn a_set_has_no_order_and_no_duplicates() {
        let matched = set(&[3, 1, 3]);
        assert!(!matched.is_sequence());
        assert_eq!(matched.len(), 2);
    }

    /// **Narrowing a sequence leaves a sequence**, because the result is a
    /// subsequence of the left operand — its order was never in question.
    #[test]
    fn narrowing_a_sequence_keeps_its_order_and_its_duplicates() {
        let out = sequence(&[3, 1, 3, 2]).combine(Op::And, &set(&[3, 2]));
        assert!(!out.demoted, "narrowing does not lose a shape");
        assert_eq!(
            out.rows.iter().collect::<Vec<_>>(),
            [song(3), song(3), song(2)],
            "the order or the duplicate did not survive"
        );
    }

    /// Excluding is the same case — #28's missing cell, and equally safe.
    #[test]
    fn excluding_from_a_sequence_keeps_its_order() {
        let out = sequence(&[3, 1, 3, 2]).combine(Op::AndNot, &set(&[1]));
        assert!(!out.demoted);
        assert_eq!(
            out.rows.iter().collect::<Vec<_>>(),
            [song(3), song(3), song(2)]
        );
    }

    /// **Widening a sequence demotes it, and the result says so.**
    ///
    /// The requirement #92 makes of this type. `crateA + crateB` has no
    /// defensible order — not concatenation, not interleaving, not a re-sort —
    /// so the shape is given up rather than one of the three being chosen
    /// quietly, and the caller is told rather than left to notice.
    #[test]
    fn widening_a_sequence_demotes_it_and_says_so() {
        let out = sequence(&[3, 1, 3]).combine(Op::Or, &set(&[2]));
        assert!(out.demoted, "a sequence was widened and nobody was told");
        assert!(!out.rows.is_sequence(), "it kept an order it cannot defend");
        assert_eq!(out.rows.len(), 3, "and the duplicate went with the order");
    }

    /// Widening a set is ordinary and loses nothing, so nothing is reported.
    #[test]
    fn widening_a_set_is_not_a_demotion() {
        let out = set(&[1, 2]).combine(Op::Or, &set(&[2, 3]));
        assert!(!out.demoted);
        assert_eq!(out.rows.len(), 3);
    }

    /// The three operators on sets are the three set operations, which is what
    /// makes `active_catalogs` an `or` and `:filter` an `and`.
    ///
    /// **Compared as sets, because that is what they are.** The first draft of
    /// this asserted a `Vec` and failed: a `BTreeSet<Id<_>>` walks in id
    /// order, and an id is a UUID, so `song(2)` before `song(3)` was a
    /// coincidence I was relying on. Asserting an order on a thing whose whole
    /// point is that it has none is the mistake this type exists to prevent,
    /// and it caught me first.
    #[test]
    fn the_operators_are_the_set_operations() {
        let (a, b) = (set(&[1, 2, 3]), set(&[2, 3, 4]));
        let held = |r: Combined| r.rows.iter().collect::<BTreeSet<_>>();
        let want = |of: &[u8]| of.iter().map(|n| song(*n)).collect::<BTreeSet<_>>();

        assert_eq!(held(a.clone().combine(Op::And, &b)), want(&[2, 3]));
        assert_eq!(held(a.clone().combine(Op::AndNot, &b)), want(&[1]));
        assert_eq!(held(a.combine(Op::Or, &b)), want(&[1, 2, 3, 4]));
    }

    /// **A sequence narrowed to nothing is still a sequence**, so the next
    /// operator behaves the same way whether or not anything matched.
    #[test]
    fn an_empty_result_keeps_the_shape_it_had() {
        let out = sequence(&[1, 2]).combine(Op::And, &set(&[9]));
        assert!(out.rows.is_sequence());
        assert_eq!(out.rows.len(), 0);
    }
}
