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
use crate::model::encoding::Format;
use crate::model::entity::{Artist, Device, Distribution, Recording, ReleaseGroup};
use crate::model::id::Id;
use crate::model::log::{Catalogue, Fact, Record, compact, fold};

/// What a scan says about one file.
///
/// The boundary between reading files and cataloguing them. #70 fills this
/// from real tags; the round trip fills it directly, because tag parsing is not
/// one of the risky decisions this slice exists to test.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scanned {
    pub audio: AudioId,
    pub title: String,
    pub artist: String,
    /// A MusicBrainz recording id, where the tags carry one — 91% of this
    /// library. Its presence is what lets two devices converge without
    /// meeting.
    pub mb_recording: Option<uuid::Uuid>,
    /// The album this sits on, as the tags name it.
    pub album: Option<String>,
    /// Who the *album* is credited to, which is not always who the track is.
    /// A guest credit must not split an album off from its artist.
    pub album_artist: Option<String>,
    /// Where on the album, as the tags claim. Claims, because they are
    /// checkable and this library has media whose numbering does not add up.
    pub disc: Option<u16>,
    pub track: Option<u16>,
    /// The release group id, where the tags carry one — what makes two devices
    /// agree on an *album* without meeting.
    pub mb_release_group: Option<uuid::Uuid>,
    /// How the audio is coded, and at what rate.
    ///
    /// Facts about the bytes, read while the file is already open. A verdict
    /// needs both — a cutoff means nothing without the bitrate that claims to
    /// justify it, and a lossless file's cutoff is a fact about the source
    /// rather than the encoder.
    pub format: Option<Format>,
    pub bitrate: u32,
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
                // No external id to converge on, so converge on the bytes
                // instead. Random minting would make two devices reading one
                // disk disagree about a song neither has any doubt about, and
                // would make the *same* device disagree with itself across a
                // rescan — which is what rung zero below was patching over.
                //
                // The cost is honest and small: a re-encode of an unbound file
                // mints a new recording, where a MusicBrainz id would have held
                // it together. That is the 8% with nothing to hold it by.
                None => known
                    .distributions
                    .get(&distribution)
                    .and_then(|d| d.recording)
                    .unwrap_or_else(|| Id::<Recording>::derived(s.audio.to_string().as_bytes())),
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
            // An album, where the tags name one. Its id derives from the
            // release group MBID when there is one, so two devices agree on
            // the *album* as well as the track; otherwise from the album
            // artist and title together, because two artists' "Greatest Hits"
            // are not one album.
            if let Some(album) = &s.album {
                let credited = s.album_artist.as_deref().unwrap_or(&s.artist);
                let group = match s.mb_release_group {
                    Some(mbid) => Id::<ReleaseGroup>::of_musicbrainz(mbid),
                    None => {
                        Id::<ReleaseGroup>::derived(format!("{credited}\u{1f}{album}").as_bytes())
                    }
                };
                let group_artist = Id::<Artist>::derived(credited.as_bytes());
                self.append(
                    now,
                    Fact::ArtistNamed {
                        artist: group_artist,
                        name: credited.to_string(),
                    },
                );
                self.append(
                    now,
                    Fact::ReleaseGroupCatalogued {
                        group,
                        title: album.clone(),
                        artist: group_artist,
                    },
                );
                self.append(
                    now,
                    Fact::TrackPlaced {
                        recording,
                        group,
                        disc: s.disc.unwrap_or(1),
                        track: s.track.unwrap_or(0),
                    },
                );
            }

            self.append(
                now,
                Fact::DistributionCatalogued {
                    distribution,
                    audio: s.audio,
                    format: s.format,
                    bitrate: s.bitrate,
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

    /// Record what an analyser found.
    ///
    /// Deliberately not part of `ingest`: measuring is minutes where hashing
    /// is seconds, and the answer only changes when the bytes or the analyser
    /// do.
    pub fn measured(
        &mut self,
        now: i64,
        distribution: Id<Distribution>,
        cutoff_hz: u32,
        analyser: u16,
    ) {
        self.append(
            now,
            Fact::Measured {
                distribution,
                cutoff_hz,
                analyser,
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
