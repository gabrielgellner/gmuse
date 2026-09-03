//! One machine's copy of the library — issue #69.
//!
//! An instance owns a clock, a log it appends to, and copies of other devices'
//! logs that arrived by sync. It has no state of its own beyond that: the
//! catalogue is [`Instance::catalogue`], folded on demand, so there is nothing
//! that can drift out of step with the record of how it got that way.
//!
//! ## What "merge" is
//!
//! Union the records, and advance the clock past what was read. The second
//! half is what makes the order causal — everything written after a sync sorts
//! after what the sync brought in — and it is why merging is a method here
//! rather than a set operation at the call site.

use std::collections::{BTreeMap, BTreeSet};

use crate::audio_id::AudioId;
use crate::model::clock::{Drift, HybridClock, Stamp};
use crate::model::entity::{Artist, Device, Distribution, Recording};
use crate::model::id::Id;
use crate::model::log::{Catalogue, Fact, Record, compact, fold};

/// What a scan says about one file.
///
/// The boundary between reading files and cataloguing them. #70 fills this
/// from real tags; the round trip fills it directly, because tag parsing is not
/// one of the risky decisions this slice exists to test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub audio: AudioId,
    pub title: String,
    pub artist: String,
    /// A MusicBrainz recording id, where the tags carry one — 83% of this
    /// library. Its presence is what lets two devices converge without
    /// meeting.
    pub mb_recording: Option<uuid::Uuid>,
}

pub struct Instance {
    clock: HybridClock,
    records: BTreeSet<Record>,
    /// Reported rather than swallowed, so a machine with a wrong clock is a
    /// thing someone gets told about.
    pub drifts: Vec<Drift>,
}

impl Instance {
    pub fn new(device: Id<Device>) -> Self {
        Instance {
            clock: HybridClock::new(device),
            records: BTreeSet::new(),
            drifts: Vec::new(),
        }
    }

    pub fn device(&self) -> Id<Device> {
        self.clock.device()
    }

    pub fn records(&self) -> &BTreeSet<Record> {
        &self.records
    }

    /// The catalogue this instance currently believes in.
    pub fn catalogue(&self) -> Catalogue {
        fold(&self.records)
    }

    fn append(&mut self, now: i64, fact: Fact) -> Stamp {
        let stamp = self.clock.stamp(now);
        self.records.insert(Record { stamp, fact });
        stamp
    }

    /// Take in what a scan found.
    ///
    /// Convergent wherever the tags allow it: the encoding's id is derived from
    /// its bytes, and the recording's from its MusicBrainz id. Without one, the
    /// recording is minted locally — which is the case §11 leaves to the
    /// collision, and the case this instance cannot resolve alone.
    pub fn ingest(&mut self, now: i64, scan: &[Scanned]) {
        // Rung zero of the ladder: have these exact bytes been bound before?
        //
        // Without it a rescan mints a fresh recording for every unbound file,
        // every pass, and the library doubles each time it is looked at. The
        // encoding's id is content-derived, so this costs one lookup and is
        // the cheapest rung there is.
        let known = self.catalogue();

        for s in scan {
            let artist = Id::<Artist>::derived(s.artist.as_bytes());
            let distribution = Id::<Distribution>::of_audio(&s.audio);
            let recording = match s.mb_recording {
                Some(mbid) => Id::<Recording>::of_musicbrainz(mbid),
                None => known
                    .distributions
                    .get(&distribution)
                    .and_then(|d| d.recording)
                    .unwrap_or_default(),
            };

            self.append(
                now,
                Fact::ArtistNamed {
                    artist,
                    name: s.artist.clone(),
                },
            );
            self.append(
                now,
                Fact::RecordingCatalogued {
                    recording,
                    title: s.title.clone(),
                    artist,
                },
            );
            self.append(
                now,
                Fact::DistributionCatalogued {
                    distribution,
                    audio: s.audio,
                },
            );
            self.append(
                now,
                Fact::DistributionBound {
                    distribution,
                    from: None,
                    to: recording,
                },
            );
        }
    }

    /// Say what this device holds, right now, in full.
    ///
    /// A scan produces the whole list, so this takes the whole list. Emitting a
    /// diff instead would mean noticing deletions, and a deletion that goes
    /// unnoticed is a row that never goes away.
    pub fn assert_holdings(&mut self, now: i64, held: BTreeMap<Id<Distribution>, String>) {
        let device = self.clock.device();
        self.append(now, Fact::HoldingsAsserted { device, held });
    }

    /// Drop this instance's superseded holdings assertions.
    ///
    /// Safe at any moment, because folding a compacted log gives exactly what
    /// folding the original gives — and safe to run on one device and not
    /// another, because a compacted record that arrives back from a stale
    /// replica is simply superseded again.
    pub fn compact(&mut self) {
        self.records = compact(&self.records);
    }

    pub fn rename_artist(&mut self, now: i64, artist: Id<Artist>, name: &str) {
        self.append(
            now,
            Fact::ArtistNamed {
                artist,
                name: name.to_string(),
            },
        );
    }

    pub fn assign_genre(&mut self, now: i64, recording: Id<Recording>, genre: &str) {
        self.append(
            now,
            Fact::GenreAssigned {
                recording,
                genre: genre.to_string(),
            },
        );
    }

    pub fn rate(&mut self, now: i64, recording: Id<Recording>, stars: Option<u8>) {
        self.append(now, Fact::Rated { recording, stars });
    }

    pub fn play(&mut self, now: i64, recording: Id<Recording>, distribution: Id<Distribution>) {
        self.append(
            now,
            Fact::Played {
                recording,
                distribution: Some(distribution),
            },
        );
    }

    /// Merge another device's records in.
    ///
    /// Idempotent, because records are identified by a stamp that is unique per
    /// device — so syncing the same log twice adds nothing, and syncing in
    /// either direction reaches the same set.
    pub fn merge(&mut self, now: i64, incoming: &BTreeSet<Record>) {
        for record in incoming {
            if let Drift::Implausible { ahead_ms } = self.clock.observe(&record.stamp, now) {
                self.drifts.push(Drift::Implausible { ahead_ms });
            }
        }
        self.records.extend(incoming.iter().cloned());
    }
}
