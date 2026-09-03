//! What we know about knowing — issue #76, `domain-model.md` §2 and §3.
//!
//! Metadata about *the entry in this catalogue*, as against metadata about the
//! music: when it was added, what identified it, how confidently, and when that
//! was last checked. DCAT calls it a `CatalogRecord` and this borrows both the
//! name and the shape.
//!
//! ## Its subject is anything, and that is the point
//!
//! An earlier draft drew it 1:1 with `Recording`. The ingest ladder mints
//! provisional **artists**, **works** and **roles** as well — at the point
//! where the least is known, which is exactly where provenance matters most.
//! Left 1:1, the record of "this was a guess" would be missing precisely where
//! there was the most guessing.
//!
//! ## Why a guess must stay marked
//!
//! Ingest never rejects, so a name that matched nothing becomes an entity
//! anyway. Without [`Confidence`] recorded beside it, that entity is
//! indistinguishable from one a MusicBrainz id confirmed — and the library
//! quietly stops being able to say which of its own contents it is sure about.

use std::collections::BTreeMap;

use crate::model::clock::Stamp;
use crate::model::ingest::Confidence;
use crate::model::reference::{Cataloguable, Source};

/// How the catalogue came to hold this, and how sure it is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogRecord {
    pub subject: Cataloguable,
    pub added: Stamp,
    /// What answered, on the ingest ladder. `None` when nothing did and the
    /// entity was minted to keep ingest total.
    pub identified_by: Option<Source>,
    pub confidence: Confidence,
    /// When a person last confirmed this, as against when a matcher last
    /// guessed it.
    pub verified: Option<Stamp>,
}

impl CatalogRecord {
    /// Whether this still wants a human to look at it.
    ///
    /// Verification settles the question however it was reached: a provisional
    /// entity someone has confirmed is no longer provisional in the only sense
    /// that matters, which is whether anyone still needs to decide about it.
    pub fn wants_review(&self) -> bool {
        self.verified.is_none() && self.confidence < Confidence::Confirmed
    }
}

/// Provenance for everything the catalogue holds.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Provenance {
    records: BTreeMap<Cataloguable, CatalogRecord>,
}

impl Provenance {
    /// Record how something came to be catalogued.
    ///
    /// Idempotent on `added`: re-seeing a file does not change when its
    /// entities entered the library, but a better identification does raise the
    /// confidence. Learning more never makes the record worse.
    pub fn note(
        &mut self,
        subject: Cataloguable,
        at: Stamp,
        identified_by: Option<Source>,
        confidence: Confidence,
    ) {
        self.records
            .entry(subject)
            .and_modify(|r| {
                if confidence > r.confidence {
                    r.confidence = confidence;
                    r.identified_by = identified_by;
                }
            })
            .or_insert(CatalogRecord {
                subject,
                added: at,
                identified_by,
                confidence,
                verified: None,
            });
    }

    /// A person has confirmed this.
    pub fn verify(&mut self, subject: Cataloguable, at: Stamp) -> bool {
        match self.records.get_mut(&subject) {
            Some(r) => {
                r.verified = Some(at);
                true
            }
            None => false,
        }
    }

    pub fn get(&self, subject: Cataloguable) -> Option<&CatalogRecord> {
        self.records.get(&subject)
    }

    /// Everything still waiting on a decision, worst first.
    ///
    /// The order is by **how much it matters**, not by how uncertain it is: an
    /// artist with 32 recordings under it is worth more of your afternoon than
    /// one with a single track, however confident the matcher felt about
    /// either. `impact` comes from the caller because provenance has no
    /// business knowing how the spine is shaped.
    ///
    /// This is `genre-uncertain.tsv` as a view rather than a file — the
    /// information it holds stops being thrown away at the end of each pass.
    pub fn review_queue(
        &self,
        impact: impl Fn(Cataloguable) -> usize,
    ) -> Vec<(&CatalogRecord, usize)> {
        let mut queue: Vec<_> = self
            .records
            .values()
            .filter(|r| r.wants_review())
            .map(|r| (r, impact(r.subject)))
            .collect();
        // Descending by impact, then by subject so the order is total and the
        // list does not shuffle between runs.
        queue.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.subject.cmp(&b.0.subject)));
        queue
    }

    /// What a merge does to provenance.
    ///
    /// The loser's record goes, because the loser is no longer something the
    /// catalogue holds — and the merge itself is recorded by the redirect
    /// (#68), which is the durable half. What survives here is the **better**
    /// of the two identifications: merging a confirmed artist with a
    /// provisional one leaves a confirmed artist, since the evidence that
    /// confirmed either still applies to the one thing they turned out to be.
    pub fn absorb(&mut self, loser: Cataloguable, winner: Cataloguable) {
        let Some(gone) = self.records.remove(&loser) else {
            return;
        };
        match self.records.get_mut(&winner) {
            Some(kept) => {
                if gone.confidence > kept.confidence {
                    kept.confidence = gone.confidence;
                    kept.identified_by = gone.identified_by;
                }
                kept.added = kept.added.min(gone.added);
                kept.verified = match (kept.verified, gone.verified) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }
            None => {
                self.records.insert(
                    winner,
                    CatalogRecord {
                        subject: winner,
                        ..gone
                    },
                );
            }
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clock::HybridClock;
    use crate::model::entity::{Artist, Device, Recording, Work};
    use crate::model::id::Id;

    fn clock() -> HybridClock {
        HybridClock::new(Id::<Device>::derived(b"laptop"))
    }

    fn artist(n: &[u8]) -> Cataloguable {
        Cataloguable::Artist(Id::<Artist>::derived(n))
    }

    /// **The audit's point.** The ladder mints provisional artists and works,
    /// not only recordings, so provenance has to reach all of them.
    #[test]
    fn anything_catalogued_can_carry_its_provenance() {
        let mut c = clock();
        let mut p = Provenance::default();
        p.note(
            Cataloguable::Recording(Id::<Recording>::derived(b"r")),
            c.stamp(100),
            Some(Source::MusicBrainz),
            Confidence::Confirmed,
        );
        p.note(
            artist(b"unknown"),
            c.stamp(101),
            None,
            Confidence::Provisional,
        );
        p.note(
            Cataloguable::Work(Id::<Work>::derived(b"w")),
            c.stamp(102),
            None,
            Confidence::Provisional,
        );

        assert_eq!(p.len(), 3);
        assert_eq!(
            p.review_queue(|_| 1).len(),
            2,
            "the two guesses, not the id"
        );
    }

    /// **The listing that replaces the TSV.** Ordered by how much it matters,
    /// so the 32-track case is above the 1-track one — the real shape of this
    /// library's duplicates.
    #[test]
    fn the_review_queue_puts_the_expensive_mistakes_first() {
        let mut c = clock();
        let mut p = Provenance::default();
        let (bodzin, antigone) = (artist(b"bodzin-vs-romboy"), artist(b"ANTIGONE"));
        p.note(bodzin, c.stamp(100), None, Confidence::Provisional);
        p.note(antigone, c.stamp(101), None, Confidence::Provisional);

        let first = |w: Cataloguable| p.review_queue(|s| if s == w { 32 } else { 1 })[0].0.subject;

        // Asked both ways round, because an order that happens to agree with
        // the ids is not an order by impact.
        assert_eq!(first(bodzin), bodzin, "32 tracks before 1");
        assert_eq!(first(antigone), antigone, "and the other way too");

        let queue = p.review_queue(|s| if s == bodzin { 32 } else { 1 });
        assert_eq!(queue[0].1, 32, "the weight comes back with the row");
    }

    /// A confirmed identification never appears in the queue, because there is
    /// nothing to decide.
    #[test]
    fn something_an_external_id_named_is_not_asking_for_attention() {
        let mut c = clock();
        let mut p = Provenance::default();
        let emmylou = artist(b"emmylou");
        p.note(
            emmylou,
            c.stamp(100),
            Some(Source::MusicBrainz),
            Confidence::Confirmed,
        );
        assert!(p.review_queue(|_| 1).is_empty());
        assert!(!p.get(emmylou).unwrap().wants_review());
    }

    /// Verification settles it however it was reached — a guess someone has
    /// checked is no longer asking anything of anyone.
    #[test]
    fn a_checked_guess_leaves_the_queue() {
        let mut c = clock();
        let mut p = Provenance::default();
        let a = artist(b"a");
        p.note(a, c.stamp(100), None, Confidence::Provisional);
        assert_eq!(p.review_queue(|_| 1).len(), 1);

        assert!(p.verify(a, c.stamp(200)));
        assert!(p.review_queue(|_| 1).is_empty());
        assert_eq!(
            p.get(a).unwrap().confidence,
            Confidence::Provisional,
            "and the record still says it began as a guess"
        );
    }

    /// Learning more never makes the record worse, and never rewrites when
    /// something entered the library.
    #[test]
    fn a_better_identification_raises_confidence_and_nothing_else() {
        let mut c = clock();
        let mut p = Provenance::default();
        let a = artist(b"a");
        let first = c.stamp(100);
        p.note(a, first, None, Confidence::Provisional);
        p.note(
            a,
            c.stamp(200),
            Some(Source::MusicBrainz),
            Confidence::Confirmed,
        );

        let r = p.get(a).unwrap();
        assert_eq!(r.confidence, Confidence::Confirmed);
        assert_eq!(r.identified_by, Some(Source::MusicBrainz));
        assert_eq!(r.added, first, "re-seeing a file does not re-date it");

        p.note(a, c.stamp(300), None, Confidence::Provisional);
        assert_eq!(
            p.get(a).unwrap().confidence,
            Confidence::Confirmed,
            "and a later worse guess does not undo it"
        );
    }

    /// **A merge keeps the better evidence.** Whatever confirmed either still
    /// applies to the one thing they turned out to be.
    #[test]
    fn merging_a_guess_into_a_confirmed_entity_stays_confirmed() {
        let mut c = clock();
        let mut p = Provenance::default();
        let (winner, loser) = (artist(b"emmylou"), artist(b"emmy-lou"));
        let early = c.stamp(50);
        p.note(loser, early, None, Confidence::Provisional);
        p.note(
            winner,
            c.stamp(100),
            Some(Source::MusicBrainz),
            Confidence::Confirmed,
        );

        p.absorb(loser, winner);

        assert!(p.get(loser).is_none(), "the loser is no longer held");
        let kept = p.get(winner).unwrap();
        assert_eq!(kept.confidence, Confidence::Confirmed);
        assert_eq!(
            kept.added, early,
            "the library has had this artist since the earlier of the two"
        );
    }

    /// And the other direction: a confirmed entity merged into a provisional
    /// one leaves the evidence intact rather than losing it.
    #[test]
    fn merging_a_confirmed_entity_into_a_guess_keeps_the_evidence() {
        let mut c = clock();
        let mut p = Provenance::default();
        let (winner, loser) = (artist(b"guess"), artist(b"known"));
        p.note(winner, c.stamp(100), None, Confidence::Provisional);
        p.note(
            loser,
            c.stamp(110),
            Some(Source::MusicBrainz),
            Confidence::Confirmed,
        );

        p.absorb(loser, winner);

        let kept = p.get(winner).unwrap();
        assert_eq!(kept.confidence, Confidence::Confirmed);
        assert_eq!(kept.identified_by, Some(Source::MusicBrainz));
        assert!(!kept.wants_review(), "nothing is left to decide");
    }

    /// The queue is ordered totally, so it does not shuffle between runs.
    #[test]
    fn the_queue_does_not_reshuffle_between_runs() {
        let mut c = clock();
        let mut p = Provenance::default();
        for n in 0..5u8 {
            p.note(
                artist(&[n]),
                c.stamp(100 + n as i64),
                None,
                Confidence::Provisional,
            );
        }
        let once: Vec<_> = p
            .review_queue(|_| 1)
            .iter()
            .map(|(r, _)| r.subject)
            .collect();
        let twice: Vec<_> = p
            .review_queue(|_| 1)
            .iter()
            .map(|(r, _)| r.subject)
            .collect();
        assert_eq!(once, twice);
    }

    #[test]
    fn provenance_round_trips_through_storage() {
        let mut c = clock();
        let mut p = Provenance::default();
        let a = artist(b"a");
        p.note(a, c.stamp(100), Some(Source::Discogs), Confidence::Matched);
        let bytes = postcard::to_allocvec(&p).unwrap();
        let back: Provenance = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.get(a), p.get(a));
    }
}
