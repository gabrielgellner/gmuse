//! What a device writes down, and what folding it back gives — issue #69.
//!
//! Every machine appends to its own log and nothing is ever rewritten, so a
//! sync has nothing to resolve: merging two devices is a **union of records**,
//! and the catalogue is what folding the union produces. That is the whole
//! mechanism `library-sync-design.md` §6 settles on, and this module is it in
//! the small.
//!
//! ## Why a structural change carries what it replaced
//!
//! `domain-model.md` §4 asks for `(field, from, to, basis)` and says a conflict
//! is "two corrections from the same base". That shape is what makes a
//! collision *detectable* rather than merely resolvable: two devices binding
//! the same encoding to different recordings both moved it **from nothing**, so
//! the shared base is what identifies them as disagreeing rather than as one
//! correcting the other.
//!
//! A clock alone cannot tell those apart. A hybrid logical clock orders
//! everything, including things that never saw each other — so ordering is
//! what decides *which value shows*, and the base is what decides *whether
//! anyone should be told*.

use std::collections::{BTreeMap, BTreeSet};

use crate::audio_id::AudioId;
use crate::model::clock::Stamp;
use crate::model::entity::{Artist, Distribution, Recording};
use crate::model::id::Id;

/// One thing a device wrote down.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub stamp: Stamp,
    pub fact: Fact,
}

/// The facts this slice needs. The catalogue's full vocabulary arrives with
/// #72–#76; these are the five that the round trip actually exercises.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fact {
    /// An artist's label. **Descriptive** — §4 Type 1, so last writer wins and
    /// there is nothing to surface.
    ArtistNamed { artist: Id<Artist>, name: String },

    /// A recording exists, and what it is called.
    RecordingCatalogued {
        recording: Id<Recording>,
        title: String,
        artist: Id<Artist>,
    },

    /// An encoding exists. Its id is derived from `audio`, so two devices
    /// reading the same bytes emit the *same* fact.
    DistributionCatalogued {
        distribution: Id<Distribution>,
        audio: AudioId,
    },

    /// Which recording an encoding holds. **Structural** — §4 Type 2, so it
    /// carries what it replaced and a disagreement is surfaced.
    DistributionBound {
        distribution: Id<Distribution>,
        from: Option<Id<Recording>>,
        to: Id<Recording>,
    },

    /// A genre on a recording. Membership is row existence, so this unions.
    GenreAssigned {
        recording: Id<Recording>,
        genre: String,
    },

    /// A play. Counted, never a stored number.
    Played {
        recording: Id<Recording>,
        distribution: Option<Id<Distribution>>,
    },
}

/// What the catalogue looks like after folding a set of records.
///
/// Every collection is ordered, because "both devices fold to the same result"
/// has to be checkable byte for byte — a hash map would make the assertion
/// pass or fail on iteration order.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Catalogue {
    pub artist_names: BTreeMap<Id<Artist>, String>,
    pub recordings: BTreeMap<Id<Recording>, Recorded>,
    pub distributions: BTreeMap<Id<Distribution>, Distributed>,
    pub genres: BTreeSet<(Id<Recording>, String)>,
    pub plays: BTreeMap<Id<Recording>, u32>,
    /// Structural disagreements. Never resolved silently — §4.
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recorded {
    pub title: String,
    pub artist: Id<Artist>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Distributed {
    pub audio: AudioId,
    pub recording: Option<Id<Recording>>,
}

/// Two devices bound one encoding to different recordings, from the same base.
///
/// The winner is shown, because a library that refuses to answer is worse than
/// one that answers and says so. The losers are kept, because "stop and show
/// me" needs something to show.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub distribution: Id<Distribution>,
    pub base: Option<Id<Recording>>,
    /// Every value asserted, ordered by stamp. The last one is what shows.
    pub asserted: Vec<(Stamp, Id<Recording>)>,
}

impl Conflict {
    pub fn winner(&self) -> Id<Recording> {
        self.asserted.last().expect("a conflict has candidates").1
    }
}

/// Every recording asserted for one `(encoding, base)` pair, in stamp order.
/// Grouping by the **base** is what distinguishes a disagreement from a
/// correction: two writes from the same base disagree, where a write from a
/// base someone else established is a correction of it.
type Bindings = BTreeMap<(Id<Distribution>, Option<Id<Recording>>), Vec<(Stamp, Id<Recording>)>>;

/// Fold a set of records into the catalogue they describe.
///
/// Sorting first is what makes the result independent of the order records
/// arrived in, which is the property the whole no-coordinator design rests on:
/// merge A into B or B into A and the answer is the same.
pub fn fold(records: &BTreeSet<Record>) -> Catalogue {
    let mut cat = Catalogue::default();
    let mut bindings: Bindings = BTreeMap::new();

    for Record { stamp, fact } in records {
        match fact {
            Fact::ArtistNamed { artist, name } => {
                // Last writer wins, and the set is already in stamp order.
                cat.artist_names.insert(*artist, name.clone());
            }
            Fact::RecordingCatalogued {
                recording,
                title,
                artist,
            } => {
                cat.recordings.insert(
                    *recording,
                    Recorded {
                        title: title.clone(),
                        artist: *artist,
                    },
                );
            }
            Fact::DistributionCatalogued {
                distribution,
                audio,
            } => {
                cat.distributions
                    .entry(*distribution)
                    .or_insert(Distributed {
                        audio: *audio,
                        recording: None,
                    });
            }
            Fact::DistributionBound {
                distribution,
                from,
                to,
            } => {
                bindings
                    .entry((*distribution, *from))
                    .or_default()
                    .push((*stamp, *to));
            }
            Fact::GenreAssigned { recording, genre } => {
                cat.genres.insert((*recording, genre.clone()));
            }
            Fact::Played { recording, .. } => {
                *cat.plays.entry(*recording).or_insert(0) += 1;
            }
        }
    }

    for ((distribution, base), mut asserted) in bindings {
        asserted.sort();
        let distinct: BTreeSet<_> = asserted.iter().map(|(_, r)| *r).collect();
        let winner = asserted.last().expect("non-empty by construction").1;

        if let Some(d) = cat.distributions.get_mut(&distribution) {
            d.recording = Some(winner);
        }
        if distinct.len() > 1 {
            cat.conflicts.push(Conflict {
                distribution,
                base,
                asserted,
            });
        }
    }
    cat.conflicts.sort_by_key(|c| c.distribution);
    cat
}

impl PartialOrd for Record {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Records order by their stamp, which is total across devices — so a set of
/// records has one iteration order everywhere.
impl Ord for Record {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.stamp.cmp(&other.stamp)
    }
}
