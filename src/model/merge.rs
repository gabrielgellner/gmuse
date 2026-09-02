//! Merging two entities that turned out to be one — issue #68,
//! `domain-model.md` §3.
//!
//! Two `Artist` rows will turn out to be the same person. The credits census
//! found 236 recordings with multi-valued artist tags, and splitting those
//! mints duplicates; the library already holds `Stephan Bodzin & Marc Romboy`
//! and `Stephan Bodzin vs. Marc Romboy` as two acts with 32 tracks each, their
//! play counts split down the middle. So merge is a first-class operation, not
//! a repair script.
//!
//! ## Nothing is destroyed
//!
//! The losing `Id<T>` becomes a **permanent redirect** to the winner rather
//! than disappearing. Every old reference keeps resolving, and **nothing in the
//! event log is rewritten** — a play recorded against the loser last year still
//! counts, because resolution happens when the reference is read.
//!
//! That is also what makes a merge a judgement rather than a fact. A redirect
//! is the honest record of a decision that could be wrong, and it can be
//! undone; a destructive merge cannot be.
//!
//! ## Two outcomes, both worth recording
//!
//! A merge proposal can be accepted *or rejected*, and only recording the first
//! is what makes a review queue useless. Without a cannot-link record, a staged
//! queue re-proposes `ANTIGONE` against `Antigone` every time it runs and never
//! converges — so a rejection is stored too, and afterwards that pair cannot be
//! merged by accident.
//!
//! ## What this does not do
//!
//! A merge usually also mints an `Alias`, so the loser's label stays findable
//! (#73). That belongs with the reference layer; this module owns identity
//! alone.

use std::collections::{HashMap, HashSet};

use crate::model::id::Id;

/// How far a chain of redirects may be followed before we call it a cycle.
///
/// Chains are short in practice — a merge of a merge is unusual and a merge of
/// a merge of a merge is a mistake. The bound exists so that data corrupted by
/// some other route cannot hang a render, which is the same discipline §5 puts
/// on derivation traversal.
const MAX_CHAIN: usize = 16;

/// Why a merge was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("an entity cannot be merged into itself")]
    Itself,
    #[error("this would make a cycle: the winner already redirects to the loser")]
    Cycle,
    #[error("these two were recorded as not the same; clear that first")]
    Barred,
    #[error("the loser is already merged into something else")]
    AlreadyMerged,
}

/// The merge decisions taken about one kind of entity.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Merges<T> {
    /// loser → winner. May chain; [`Merges::resolve`] follows it.
    redirect: HashMap<Id<T>, Id<T>>,
    /// Pairs a person said are *not* the same thing, held in a stable order so
    /// that a bar recorded one way round is found the other.
    barred: HashSet<(Id<T>, Id<T>)>,
}

impl<T> Default for Merges<T> {
    fn default() -> Self {
        Merges {
            redirect: HashMap::new(),
            barred: HashSet::new(),
        }
    }
}

impl<T> Merges<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `loser` and `winner` are one entity, and that the winner is
    /// the name it keeps.
    pub fn merge(&mut self, loser: Id<T>, winner: Id<T>) -> Result<(), MergeError> {
        if loser == winner {
            return Err(MergeError::Itself);
        }
        if self.redirect.contains_key(&loser) {
            return Err(MergeError::AlreadyMerged);
        }
        if self.is_barred(loser, winner) {
            return Err(MergeError::Barred);
        }
        // Following the winner first is what makes A→B, B→A impossible, and it
        // is also what makes merging into an already-merged entity land on the
        // survivor rather than on a dead id.
        let winner = self.resolve(winner);
        if winner == loser {
            return Err(MergeError::Cycle);
        }
        self.redirect.insert(loser, winner);
        Ok(())
    }

    /// The entity this id names *now*.
    ///
    /// Follows a chain, so an id merged into one that was later merged again
    /// still lands on the survivor. An id nothing was ever done to resolves to
    /// itself, which is the common case and costs one lookup.
    pub fn resolve(&self, id: Id<T>) -> Id<T> {
        let mut current = id;
        for _ in 0..MAX_CHAIN {
            match self.redirect.get(&current) {
                Some(&next) => current = next,
                None => return current,
            }
        }
        // Only reachable from data this module refuses to create. Stopping is
        // the point: a bounded wrong answer beats an unbounded loop.
        current
    }

    /// Whether this id has been merged away.
    pub fn is_merged(&self, id: Id<T>) -> bool {
        self.redirect.contains_key(&id)
    }

    /// Undo one merge. The reason redirects are preferred to deletion.
    pub fn undo(&mut self, loser: Id<T>) -> bool {
        self.redirect.remove(&loser).is_some()
    }

    /// Record that two entities are **not** the same, so nothing proposes them
    /// again.
    pub fn bar(&mut self, a: Id<T>, b: Id<T>) -> Result<(), MergeError> {
        if a == b {
            return Err(MergeError::Itself);
        }
        if self.resolve(a) == self.resolve(b) {
            return Err(MergeError::AlreadyMerged);
        }
        self.barred.insert(Self::pair(a, b));
        Ok(())
    }

    pub fn is_barred(&self, a: Id<T>, b: Id<T>) -> bool {
        self.barred.contains(&Self::pair(a, b))
    }

    /// Withdraw a rejection, so the pair can be proposed again.
    pub fn unbar(&mut self, a: Id<T>, b: Id<T>) -> bool {
        self.barred.remove(&Self::pair(a, b))
    }

    /// Every id that has been merged away, with what it now resolves to.
    pub fn redirects(&self) -> impl Iterator<Item = (Id<T>, Id<T>)> + '_ {
        self.redirect
            .iter()
            .map(|(&from, _)| (from, self.resolve(from)))
    }

    pub fn is_empty(&self) -> bool {
        self.redirect.is_empty() && self.barred.is_empty()
    }

    /// Order-independent, so `bar(a, b)` and `bar(b, a)` are one decision.
    fn pair(a: Id<T>, b: Id<T>) -> (Id<T>, Id<T>) {
        if a <= b { (a, b) } else { (b, a) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Artist;

    fn artist(n: u128) -> Id<Artist> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// The point of the whole module: an old reference keeps resolving, so
    /// nothing that ever named the loser has to be rewritten.
    #[test]
    fn an_old_id_still_finds_the_entity() {
        let (loser, winner) = (artist(1), artist(2));
        let mut merges = Merges::new();
        merges.merge(loser, winner).unwrap();

        assert_eq!(merges.resolve(loser), winner);
        assert_eq!(merges.resolve(winner), winner, "the survivor is itself");
        assert!(merges.is_merged(loser) && !merges.is_merged(winner));
    }

    /// An id nobody has touched costs one lookup and answers itself.
    #[test]
    fn an_untouched_id_resolves_to_itself() {
        let merges: Merges<Artist> = Merges::new();
        assert_eq!(merges.resolve(artist(7)), artist(7));
    }

    /// A merge of a merge lands on the survivor, not on a dead id.
    #[test]
    fn a_chain_resolves_to_the_last_survivor() {
        let (a, b, c) = (artist(1), artist(2), artist(3));
        let mut merges = Merges::new();
        merges.merge(a, b).unwrap();
        merges.merge(b, c).unwrap();
        assert_eq!(merges.resolve(a), c, "A was merged into B, and B into C");
    }

    /// Merging into something already merged away resolves the target first,
    /// so no redirect is ever written pointing at a dead id.
    #[test]
    fn merging_into_a_merged_entity_lands_on_the_survivor() {
        let (a, b, c) = (artist(1), artist(2), artist(3));
        let mut merges = Merges::new();
        merges.merge(b, c).unwrap();
        merges.merge(a, b).unwrap();
        assert_eq!(merges.resolve(a), c);
    }

    /// A cycle would make resolution meaningless, so it is refused where it
    /// would be created rather than survived where it would be read.
    #[test]
    fn a_cycle_is_refused_at_the_point_it_would_be_made() {
        let (a, b) = (artist(1), artist(2));
        let mut merges = Merges::new();
        merges.merge(a, b).unwrap();
        assert_eq!(merges.merge(b, a), Err(MergeError::Cycle));
        assert_eq!(merges.resolve(a), b, "and nothing was disturbed");
    }

    #[test]
    fn the_obvious_mistakes_are_refused() {
        let mut merges = Merges::new();
        assert_eq!(merges.merge(artist(1), artist(1)), Err(MergeError::Itself));
        merges.merge(artist(1), artist(2)).unwrap();
        assert_eq!(
            merges.merge(artist(1), artist(3)),
            Err(MergeError::AlreadyMerged),
            "a loser has exactly one winner"
        );
    }

    /// **Why a redirect and not a deletion.** A merge is a judgement, and a
    /// judgement can be wrong.
    #[test]
    fn a_merge_can_be_undone() {
        let (loser, winner) = (artist(1), artist(2));
        let mut merges = Merges::new();
        merges.merge(loser, winner).unwrap();
        assert!(merges.undo(loser));

        assert_eq!(merges.resolve(loser), loser, "it is its own entity again");
        assert!(!merges.undo(loser), "undoing twice changes nothing");
    }

    /// Without this a staged review queue re-proposes the same rejected pair
    /// forever and never converges. `ANTIGONE` and `Antigone` are probably one
    /// act — but Antigone is also a French techno producer.
    #[test]
    fn a_rejected_pair_stays_rejected() {
        let (a, b) = (artist(1), artist(2));
        let mut merges = Merges::new();
        merges.bar(a, b).unwrap();

        assert!(merges.is_barred(a, b) && merges.is_barred(b, a));
        assert_eq!(merges.merge(a, b), Err(MergeError::Barred));
        assert_eq!(
            merges.merge(b, a),
            Err(MergeError::Barred),
            "the bar is about the pair, not the direction"
        );
    }

    /// A rejection is a decision too, so withdrawing it is deliberate.
    #[test]
    fn a_rejection_can_be_withdrawn() {
        let (a, b) = (artist(1), artist(2));
        let mut merges = Merges::new();
        merges.bar(a, b).unwrap();
        assert!(merges.unbar(b, a), "found regardless of order");
        merges.merge(a, b).expect("mergeable once the bar is gone");
    }

    #[test]
    fn barring_something_already_merged_is_a_contradiction() {
        let (a, b) = (artist(1), artist(2));
        let mut merges = Merges::new();
        merges.merge(a, b).unwrap();
        assert_eq!(merges.bar(a, b), Err(MergeError::AlreadyMerged));
    }

    /// Decisions persist, so they survive the process that took them.
    #[test]
    fn decisions_round_trip_through_storage() {
        let (a, b, c, d) = (artist(1), artist(2), artist(3), artist(4));
        let mut merges = Merges::new();
        merges.merge(a, b).unwrap();
        merges.bar(c, d).unwrap();

        let bytes = postcard::to_allocvec(&merges).unwrap();
        let back: Merges<Artist> = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(back.resolve(a), b);
        assert!(back.is_barred(d, c));
    }

    /// The listing a review surface reads, already resolved through chains.
    #[test]
    fn redirects_can_be_listed_fully_resolved() {
        let (a, b, c) = (artist(1), artist(2), artist(3));
        let mut merges = Merges::new();
        merges.merge(a, b).unwrap();
        merges.merge(b, c).unwrap();

        let mut listed: Vec<_> = merges.redirects().collect();
        listed.sort();
        assert_eq!(listed, vec![(a, c), (b, c)], "both point at the survivor");
    }
}
