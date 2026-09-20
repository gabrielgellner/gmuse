//! **L1 — the projection**, and how far it has folded.
//!
//! `access-layers.md` §3. The librarian's read model: derived from the log,
//! disposable, and rebuildable from nothing. What it adds over calling
//! [`crate::projection::catalogue::fold`] is that it **knows where it got to**, which is
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
//! ## How `advance` decides
//!
//! `advance` is handed the whole record set and finds what it does not cover;
//! `apply` is handed only what arrived and does the same, refusing rather than
//! refolding when it cannot (#269) — the engine holds no records to refold
//! from, and reads the log back for exactly that. Then, in order (#267, the
//! order #261 settled):
//!
//! 1. **A record stamped below anything already folded is a rebuild.** The fold
//!    is correct because it runs in stamp order — every last-writer-wins map
//!    depends on it — and a record from a slower clock applied in arrival order
//!    would have the last word it does not deserve. The log has one device
//!    today, so this fires only when sync brings another's records.
//! 2. **A wide fact is a rebuild.** The merges, and the three spine facts that a
//!    placement's rung is computed from (`ReleaseCatalogued`, `MediumCatalogued`,
//!    `TrackPlaced`). Their reach is not derivable from the fact.
//! 3. **Everything else is applied in place**, by the same [`catalogue::apply_one`]
//!    the fold runs, and then ranked for its own key by the same
//!    [`catalogue::rank_placement`] and [`catalogue::rank_binding`] the fold
//!    runs for every key. One implementation, so the two cannot drift.
//!
//! `past` is the net: every shape that reaches backwards, in arrival order,
//! checked against a rebuild batch by batch. Read it before changing any of
//! the three rules.

// **The model ahead of its callers**, which is this project's ordinary
// sequence: #194 persists a projection and is the first thing that has to ask
// where it got to, #195 is the first read that trusts the answer.
#![allow(
    dead_code,
    reason = "L1's contract lands before its callers — #194 persists it, #195 reads through it"
)]

pub mod catalogue;
pub mod heard;
#[cfg(test)]
mod past;

use std::collections::{BTreeMap, BTreeSet};

use crate::facts::{Blast, Fact, Record, Touched};
use crate::model::clock::Stamp;
use crate::model::entity::Device;
use crate::model::id::Id;
use crate::projection::catalogue::{Catalogue, apply_one, fold, rank_binding, rank_placement};

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

    /// Whether one stamp is already folded.
    fn covers_stamp(&self, stamp: &Stamp) -> bool {
        self.0.get(&stamp.device).is_some_and(|held| held >= stamp)
    }

    /// The newest stamp folded from any device — the watermark a slower
    /// clock's record falls below.
    fn newest(&self) -> Option<Stamp> {
        self.0.values().max().copied()
    }

    /// Take the other position's watermarks where they are ahead.
    ///
    /// **Never behind**, which is what lets a projection be advanced from a
    /// *tail* rather than the whole log (#269): the tail's own position names
    /// only the devices it holds records for, and replacing this one with it
    /// would forget every other device's watermark.
    fn absorb(&mut self, other: &Position) {
        for (device, stamp) in &other.0 {
            self.0
                .entry(*device)
                .and_modify(|held| {
                    if stamp > held {
                        *held = *stamp;
                    }
                })
                .or_insert(*stamp);
        }
    }

    /// The stamp folded for one device, if any.
    pub fn of_device(&self, device: Id<Device>) -> Option<Stamp> {
        self.0.get(&device).copied()
    }

    /// Every device and where it was folded to.
    pub fn each(&self) -> impl Iterator<Item = (Id<Device>, Stamp)> + '_ {
        self.0.iter().map(|(d, s)| (*d, *s))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advanced {
    /// Already covered the records offered. One comparison, no work.
    AlreadyThere,
    /// Refolded from every record, and this is where it got to.
    Folded,
    /// **Could not be applied in place, and was not refolded either** — #269.
    /// Only [`Projected::apply`] says this: it is handed a tail and has
    /// nothing to refold from, so the owner of the log has to. The reason is
    /// carried so the owner can say why a keystroke cost a fold.
    Refused(Refusal),
    /// Applied in place, and this is what the facts touched — none of them
    /// wide, none of them from below the watermark — #267. What it names is
    /// what `Changed::These` names, so a publish needs no before-picture of
    /// the log to say what moved (#269).
    Applied(Vec<Touched>),
}

/// Why a fact could not be applied in place — the two rules of the three
/// that end in a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A record stamped below something already folded: a slower clock's,
    /// which a stamp-ordered fold would have placed earlier.
    BelowTheWatermark,
    /// A fact whose reach is not derivable from it.
    Wide,
}

impl Advanced {
    /// How many facts were applied in place; zero for the others.
    pub fn applied(&self) -> usize {
        match self {
            Advanced::Applied(touched) => touched.len(),
            _ => 0,
        }
    }
}

/// A folded catalogue that knows how far it folded.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Projected {
    catalogue: Catalogue,
    at: Position,
}

impl Projected {
    /// **Fold from nothing.** Always available, which is the invariant that
    /// makes every later change to the projection's shape a version bump
    /// rather than a migration.
    /// **Crate-private, so `advance` is the only way in.** A caller that
    /// folded straight from the log would be folding without asking whether
    /// it needed to, which is the whole of what a position is for.
    pub(crate) fn rebuild(records: &BTreeSet<Record>) -> Self {
        Projected {
            catalogue: fold(records),
            at: Position::of(records),
        }
    }

    /// **Bring it up to date, or find out it already is.**
    ///
    /// The case that pays most often is the cheapest: a read that is caught up
    /// costs a comparison over one stamp per device, not the 46ms that folding
    /// 279,000 records costs, and that is what lets a query advance freely
    /// (§3a) instead of a person having to know when to.
    ///
    /// The case a write pays is the next one: the facts that arrived, applied in
    /// place. The module doc has the three rules; `past` is what holds them.
    pub fn advance(&mut self, records: &BTreeSet<Record>) -> Advanced {
        match self.apply(records) {
            Advanced::Refused(_) => {
                *self = Projected::rebuild(records);
                Advanced::Folded
            }
            done => done,
        }
    }

    /// **Apply what arrived, or say why it cannot be** — #269.
    ///
    /// The half of [`advance`](Self::advance) that needs no log: `fresh` may
    /// be the whole record set or only the tail that was just read, and a
    /// projection advanced from tails never holds — or needs — the records
    /// behind it. The position absorbs the tail's watermarks rather than
    /// taking them, so a device the tail has nothing from is not forgotten.
    ///
    /// It never refolds. A refusal is the caller's to answer, because only
    /// the owner of the log knows where the whole of it is — on disk, for
    /// the engine, which reads it back for exactly this and lets it go again.
    pub fn apply(&mut self, fresh: &BTreeSet<Record>) -> Advanced {
        let head = Position::of(fresh);
        if self.at.covers(&head) {
            return Advanced::AlreadyThere;
        }
        let watermark = self.at.newest();
        let mut arrived = Vec::new();
        for record in fresh {
            if self.at.covers_stamp(&record.stamp) {
                continue;
            }
            if watermark.is_some_and(|newest| record.stamp < newest) {
                return Advanced::Refused(Refusal::BelowTheWatermark);
            }
            if record.fact.blast() != Blast::Narrow {
                return Advanced::Refused(Refusal::Wide);
            }
            arrived.push(record);
        }
        // Stamp order, which `BTreeSet` iteration already is — and the loop above
        // has just proved every one of these is above everything folded.
        let mut touched = Vec::with_capacity(arrived.len());
        for Record { stamp, fact } in &arrived {
            touched.extend(fact.touches());
            apply_one(&mut self.catalogue, stamp, fact);
            match fact {
                Fact::EncodingPlaced { encoding, .. } => {
                    rank_placement(&mut self.catalogue, *encoding);
                }
                Fact::EncodingBound { encoding, from, .. } => {
                    let key = (*encoding, from.map(|f| self.catalogue.merges.resolve(f)));
                    rank_binding(&mut self.catalogue, key);
                }
                _ => {}
            }
        }
        self.at.absorb(&head);
        Advanced::Applied(touched)
    }

    /// How far the log has been folded in. The whole of staleness.
    pub fn position(&self) -> &Position {
        &self.at
    }

    /// **Take the catalogue and drop the rest** — #261.
    ///
    /// For a follower that wants the fold and not the position it was folded at.
    /// `Catalogue` is deliberately not `Clone` — a copy of the library is what this whole
    /// milestone is about — so a caller that needs to own one consumes the projection.
    pub fn into_catalogue(self) -> Catalogue {
        self.catalogue
    }

    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Fact;
    use crate::model::clock::HybridClock;
    use crate::model::entity::Artist;
    use crate::model::instance::{Instance, Scanned};

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

    /// **A snapshot round-trips, and a caught-up one needs no fold** — #194.
    ///
    /// The whole of what persistence buys: a process that starts from a
    /// snapshot is already where the last one got to, so `advance` is one
    /// comparison per device rather than a fold of the log.
    #[test]
    fn a_snapshot_comes_back_where_it_was_left() {
        let dir = std::env::temp_dir().join("gmuse-projection-snapshot");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("projection.postcard");

        let records = authored(device(1), 8, 1_000);
        let folded = Projected::rebuild(&records);
        crate::cache::save_projection(&at, &crate::cache::LogPrint::default(), &folded);

        let mut back = crate::cache::current_projection(&at, &crate::cache::LogPrint::default())
            .unwrap_or_default();
        assert_eq!(back.position(), folded.position(), "it came back behind");
        assert_eq!(
            back.catalogue(),
            folded.catalogue(),
            "and saying something else"
        );
        assert_eq!(
            back.advance(&records),
            Advanced::AlreadyThere,
            "a loaded snapshot should already cover what made it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Missing, corrupt and stale are one answer, and it is a fold.**
    ///
    /// A snapshot is derived, so the only thing it can cost is time. What it
    /// must never do is hand back something that parses but is not what was
    /// written — which postcard makes possible, because it is not
    /// self-describing, and which is why the version is read before the
    /// payload (#190).
    #[test]
    fn a_snapshot_that_cannot_be_trusted_is_not_used() {
        let dir = std::env::temp_dir().join("gmuse-projection-bad");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("projection.postcard");

        // Nothing there.
        assert!(
            crate::cache::current_projection(&at, &crate::cache::LogPrint::default())
                .unwrap_or_default()
                .position()
                .is_empty()
        );

        // Rubbish.
        std::fs::write(&at, b"not a snapshot").unwrap();
        assert!(
            crate::cache::current_projection(&at, &crate::cache::LogPrint::default())
                .unwrap_or_default()
                .position()
                .is_empty()
        );

        // Right magic, wrong version, payload that would otherwise parse.
        let records = authored(device(1), 3, 1_000);
        crate::cache::save_projection(
            &at,
            &crate::cache::LogPrint::default(),
            &Projected::rebuild(&records),
        );
        let good = std::fs::read(&at).unwrap();
        let mut stale = b"gmuse".to_vec();
        stale.extend_from_slice(&postcard::to_allocvec(&99_u32).unwrap());
        stale.extend_from_slice(&good[5 + 1..]);
        std::fs::write(&at, &stale).unwrap();
        assert!(
            crate::cache::current_projection(&at, &crate::cache::LogPrint::default())
                .unwrap_or_default()
                .position()
                .is_empty(),
            "a snapshot from another version must not be believed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A snapshot older than the log is caught up, not thrown away.**
    ///
    /// The ordinary case once this is wired in: something appended since the
    /// last run, and the gap is what gets folded rather than the whole log.
    #[test]
    fn a_snapshot_behind_the_log_advances_to_it() {
        let dir = std::env::temp_dir().join("gmuse-projection-behind");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("projection.postcard");

        let d = device(1);
        let first = authored(d, 4, 1_000);
        crate::cache::save_projection(
            &at,
            &crate::cache::LogPrint::default(),
            &Projected::rebuild(&first),
        );

        let mut both = first.clone();
        both.extend(authored(d, 9, 5_000));

        let mut back = crate::cache::current_projection(&at, &crate::cache::LogPrint::default())
            .unwrap_or_default();
        // Nine narrow facts above the watermark: applied, not folded (#267).
        assert_eq!(back.advance(&both).applied(), 9);
        assert_eq!(
            back.catalogue(),
            Projected::rebuild(&both).catalogue(),
            "advancing from a snapshot did not land where rebuilding would"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(advanced.advance(&both).applied(), 7);

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

        // **`Applied(1)`, and that is the instrument** — #267's done-when. A
        // rating is narrow and newest, so it is applied to the catalogue in
        // place; `Folded` here would mean the whole log was replayed to land
        // one fact, which is what this issue exists to stop.
        assert_eq!(
            p.advance(i.records()),
            Advanced::Applied(vec![crate::facts::Touched::Recording(recording)]),
            "a rating was appended and the projection folded everything, or nothing"
        );
        assert_eq!(
            p.catalogue(),
            Projected::rebuild(i.records()).catalogue(),
            "the edit did not land where a rebuild would put it"
        );
    }
}
