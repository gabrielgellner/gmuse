//! **L1 — the projection**, and how far it has folded.
//!
//! `access-layers.md` §3. The librarian's read model: derived from the log,
//! disposable, and rebuildable from nothing. What it adds over calling
//! [`crate::model::log::fold`] is that it **knows where it got to**, which is
//! the whole of staleness and the thing that lets a read cost an integer
//! comparison instead of a fold.
//!
//! ## Why the position is per device
//!
//! A single watermark would be wrong, and quietly. `Instance::merge` extends
//! the record set with whatever a sync brought in, and those records carry the
//! stamps of the device that wrote them — routinely **below** the highest
//! stamp already folded. A "everything above the maximum" rule skips them
//! entirely and the projection is short a day's listening with nothing to say
//! so.
//!
//! So a position is one watermark **per device**, which is the shape
//! [`crate::model::durable::high_water`] already computes for flushing. Two
//! positions compare by covering rather than by ordering: `a` covers `b` when
//! it has folded at least as far for every device `b` names.
//!
//! ## What is not here yet
//!
//! `advance` refolds when it is behind. That makes `advance ≡ rebuild` true by
//! construction rather than by test — deliberately, because the test is what
//! any incremental version must be checked against and it is worth locking in
//! first. The case that already pays is the other one: a caught-up read costs
//! one comparison per device rather than 44ms.
//!
//! Applying **one fact** to an existing projection is the next step and needs
//! #192's tiers. It is not a slice of [`fold`]: that pass applies merges
//! first and resolves everything through the finished set, because a merge
//! recorded on Tuesday has to move a rating given on Monday. An incremental
//! path is therefore a second implementation of those semantics, which is the
//! real cost of it — and the equivalence test is what makes paying it safe.

// **The model ahead of its callers**, which is this project's ordinary
// sequence: #194 persists a projection and is the first thing that has to ask
// where it got to, #195 is the first read that trusts the answer.
#![expect(
    dead_code,
    reason = "L1's contract lands before its callers — #194 persists it, #195 reads through it"
)]

use std::collections::{BTreeMap, BTreeSet};

use crate::model::clock::Stamp;
use crate::model::entity::Device;
use crate::model::id::Id;
use crate::model::log::{Catalogue, Record, fold};

/// How far a projection has folded, per device.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position(BTreeMap<Id<Device>, Stamp>);

impl Position {
    /// The position a set of records represents: the highest stamp each device
    /// wrote in it.
    pub fn of(records: &BTreeSet<Record>) -> Self {
        let mut out: BTreeMap<Id<Device>, Stamp> = BTreeMap::new();
        for Record { stamp, .. } in records {
            out.entry(stamp.device)
                .and_modify(|held| {
                    if stamp > held {
                        *held = *stamp;
                    }
                })
                .or_insert(*stamp);
        }
        Position(out)
    }

    /// Whether this has folded at least as far as `other`, for every device
    /// `other` names.
    ///
    /// **Not an ordering.** Two devices can each be ahead of the other on their
    /// own records, which is ordinary rather than exceptional — it is what two
    /// machines listening to music look like. "Am I behind" is a question about
    /// coverage and this is it.
    pub fn covers(&self, other: &Position) -> bool {
        other
            .0
            .iter()
            .all(|(device, stamp)| self.0.get(device).is_some_and(|held| held >= stamp))
    }

    /// How many devices this has folded anything from.
    pub fn devices(&self) -> usize {
        self.0.len()
    }

    /// Nothing folded yet.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What an [`Projected::advance`] turned out to be.
///
/// Reported rather than inferred, because "it did nothing" is the case a read
/// depends on being cheap and the only way to assert it is to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advanced {
    /// Already covered the records offered. One comparison, no work.
    AlreadyThere,
    /// Folded, and this is where it got to.
    Folded,
}

/// A folded catalogue that knows how far it folded.
#[derive(Debug, Default)]
pub struct Projected {
    catalogue: Catalogue,
    at: Position,
}

impl Projected {
    /// **Fold from nothing.** Always available, which is the invariant that
    /// makes every later change to the projection's shape a version bump
    /// rather than a migration.
    pub fn rebuild(records: &BTreeSet<Record>) -> Self {
        Projected {
            catalogue: fold(records),
            at: Position::of(records),
        }
    }

    /// **Bring it up to date, or find out it already is.**
    ///
    /// Today this refolds when it is behind, which makes `advance ≡ rebuild`
    /// true by construction rather than by test — deliberately, because the
    /// test is what any incremental version has to be checked against and it
    /// is worth locking in before the optimisation exists.
    ///
    /// The case that already pays is the other one. A read that is caught up
    /// costs a comparison over one stamp per device, not the 44ms that folding
    /// 279,000 records costs, and that is what lets a query advance freely
    /// (§3a) instead of a person having to know when to.
    ///
    /// **Incremental application is the next step and needs #192's tiers:**
    /// a narrow fact can be applied to the rows it names, and a wide one —
    /// where the affected set is not derivable from the fact — refolds. The
    /// fold is multi-pass (merges first, then everything resolved through the
    /// finished set), so an incremental path is a second implementation of its
    /// semantics rather than a slice of it. That is the cost, and the
    /// equivalence test is what makes paying it safe.
    pub fn advance(&mut self, records: &BTreeSet<Record>) -> Advanced {
        let head = Position::of(records);
        if self.at.covers(&head) {
            return Advanced::AlreadyThere;
        }
        *self = Projected::rebuild(records);
        Advanced::Folded
    }

    /// How far the log has been folded in. The whole of staleness.
    pub fn position(&self) -> &Position {
        &self.at
    }

    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clock::HybridClock;
    use crate::model::entity::Artist;
    use crate::model::instance::{Instance, Scanned};
    use crate::model::log::Fact;

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// `n` records from one device, stamped by one clock.
    fn authored(d: Id<Device>, n: u128, from: i64) -> BTreeSet<Record> {
        let mut clock = HybridClock::new(d);
        (0..n)
            .map(|i| Record {
                stamp: clock.stamp(from + i as i64),
                fact: Fact::ArtistNamed {
                    artist: Id::<Artist>::from_uuid(uuid::Uuid::from_u128(i)),
                    name: format!("artist {i}"),
                },
            })
            .collect()
    }

    /// **A caught-up projection does no work, and that is the point.**
    ///
    /// Folding 279,000 records costs 44ms on the live library. A read may
    /// advance freely (`access-layers.md` §3a) only because being already
    /// there is a comparison over one stamp per device — if this ever starts
    /// folding, every query pays for it and nothing says so.
    #[test]
    fn advancing_a_caught_up_projection_does_nothing() {
        let records = authored(device(1), 6, 1_000);
        let mut p = Projected::rebuild(&records);

        assert_eq!(p.advance(&records), Advanced::AlreadyThere);
        assert_eq!(p.advance(&records), Advanced::AlreadyThere);
        assert_eq!(p.position(), &Position::of(&records));
    }

    /// **`advance` ends where `rebuild` would have.**
    ///
    /// The property every incremental version has to be checked against, and
    /// the reason it is written now while it is true by construction: it is
    /// the safety net for changing the projection's shape (#193) and for
    /// applying one fact at a time later.
    #[test]
    fn advancing_lands_where_rebuilding_would() {
        let d = device(1);
        let first = authored(d, 4, 1_000);
        let mut clock = HybridClock::new(d);
        for _ in 0..4 {
            let _ = clock.stamp(1_000);
        }
        let mut both = first.clone();
        both.extend(authored(d, 7, 5_000));

        let mut advanced = Projected::rebuild(&first);
        assert_eq!(advanced.advance(&both), Advanced::Folded);

        let rebuilt = Projected::rebuild(&both);
        assert_eq!(
            advanced.catalogue(),
            rebuilt.catalogue(),
            "advancing did not land where rebuilding would"
        );
        assert_eq!(advanced.position(), rebuilt.position());
    }

    /// **Folding the same records twice changes nothing.**
    ///
    /// Idempotence is what makes a fold that stops halfway resumable without a
    /// special case, and the model already has it: facts are row existence or
    /// last-writer-wins, so nothing here can disagree with itself.
    #[test]
    fn folding_twice_is_folding_once() {
        let records = authored(device(1), 9, 1_000);
        let once = Projected::rebuild(&records);
        let twice = Projected::rebuild(&records);
        assert_eq!(once.catalogue(), twice.catalogue());
        assert_eq!(once.position(), twice.position());
    }

    /// **A sync brings records from below the watermark, and they are not
    /// skipped.**
    ///
    /// The failure a single watermark would have, and the reason a position is
    /// per device. `Instance::merge` extends the set with whatever arrived,
    /// and another machine's stamps are routinely *older* than the highest one
    /// already folded — a device that has been off for a week writes a week of
    /// records below everything this one has.
    #[test]
    fn records_from_another_device_are_not_behind_the_watermark() {
        let mine = authored(device(1), 5, 9_000);
        let theirs = authored(device(2), 5, 1_000);

        let mut p = Projected::rebuild(&mine);
        assert!(
            !p.position().covers(&Position::of(&theirs)),
            "a device never heard from cannot already be covered"
        );

        let mut both = mine.clone();
        both.extend(theirs.iter().cloned());
        assert_eq!(
            p.advance(&both),
            Advanced::Folded,
            "older stamps from a second device still have to be folded"
        );
        assert_eq!(p.position().devices(), 2);
        assert_eq!(p.catalogue(), Projected::rebuild(&both).catalogue());
    }

    /// **One appended fact leaves the projection one fact behind**, which is
    /// the case the TUI's edit path is (`access-layers.md` §3a): append,
    /// flush, advance, redraw.
    #[test]
    fn an_edit_leaves_the_projection_exactly_one_fact_behind() {
        let d = device(1);
        let mut i = Instance::new(d);
        i.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 3 },
                title: "Queen in My Pictures".into(),
                artist: "Animal Collective".into(),
                ..Default::default()
            }],
        );
        let mut p = Projected::rebuild(i.records());
        assert_eq!(p.advance(i.records()), Advanced::AlreadyThere);

        let recording = *p
            .catalogue()
            .recordings
            .keys()
            .next()
            .expect("the ingest catalogued one");
        i.append_fact(
            200,
            Fact::RecordingRated {
                recording,
                stars: Some(4),
            },
        );

        assert_eq!(
            p.advance(i.records()),
            Advanced::Folded,
            "a fact was appended and the projection did not notice"
        );
        assert_eq!(
            p.catalogue(),
            Projected::rebuild(i.records()).catalogue(),
            "the edit did not land where a rebuild would put it"
        );
    }
}
