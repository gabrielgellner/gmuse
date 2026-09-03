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
use std::path::Path;

use crate::audio_id::AudioId;
use crate::model::clock::{Drift, HybridClock, Stamp};
use crate::model::durable::{self, Damage, LogError};
use crate::model::encoding::Format;
use crate::model::entity::{Artist, Device, Encoding, Recording, ReleaseGroup};
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
    /// A MusicBrainz recording id, where the tags carry one — 92.7% of this
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
    /// The highest stamp of ours already on disk, so a flush knows where to
    /// resume. `None` until this device has written anything.
    flushed: Option<Stamp>,
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
            flushed: None,
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
            let encoding = Id::<Encoding>::of_audio(&s.audio);
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
                // it together. That is the 7.3% with nothing to hold it by.
                None => known
                    .encodings
                    .get(&encoding)
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
                Fact::EncodingCatalogued {
                    encoding,
                    audio: s.audio,
                    format: s.format,
                    bitrate: s.bitrate,
                },
            );
            self.append(
                now,
                Fact::EncodingBound {
                    encoding,
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
    pub fn assert_holdings(&mut self, now: i64, held: BTreeMap<Id<Encoding>, String>) {
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
    pub fn measured(&mut self, now: i64, encoding: Id<Encoding>, cutoff_hz: u32, analyser: u16) {
        self.append(
            now,
            Fact::Measured {
                encoding,
                cutoff_hz,
                analyser,
            },
        );
    }

    pub fn rate(&mut self, now: i64, recording: Id<Recording>, stars: Option<u8>) {
        self.append(now, Fact::Rated { recording, stars });
    }

    /// Record when a recording entered the library.
    ///
    /// Idempotent by construction: the fold takes the minimum, so saying it
    /// twice, or saying it later than another device did, changes nothing.
    pub fn acquire(&mut self, now: i64, recording: Id<Recording>, at: i64) {
        self.append(now, Fact::Acquired { recording, at });
    }

    /// A play whose encoding may not be known.
    ///
    /// The recording is what the play is *about*; the encoding pins which
    /// bytes it came from and is worth having but is not what makes the fact
    /// meaningful. A file gmuse could not hash still played.
    pub fn play_maybe(
        &mut self,
        now: i64,
        recording: Id<Recording>,
        encoding: Option<Id<Encoding>>,
    ) {
        self.append(
            now,
            Fact::Played {
                recording,
                encoding,
            },
        );
    }

    pub fn play(&mut self, now: i64, recording: Id<Recording>, encoding: Id<Encoding>) {
        self.append(
            now,
            Fact::Played {
                recording,
                encoding: Some(encoding),
            },
        );
    }

    /// Merge another device's records in.
    ///
    /// Idempotent, because records are identified by a stamp that is unique per
    /// device — so syncing the same log twice adds nothing, and syncing in
    /// either direction reaches the same set.
    /// Load this device's view of the library from disk.
    ///
    /// **The clock is wound forward past everything it reads.** Without that,
    /// a device that has been off writes a stamp below records that arrived
    /// while it was away, and the fold decides a six-month-old rating is the
    /// latest one. `observe` is the step `(device, seq)` never had, and this is
    /// the moment it matters most — a launch reads every other device's log at
    /// once.
    ///
    /// A first run is an empty directory, not an error. Damaged lines come back
    /// alongside rather than stopping the load.
    pub fn open(data_dir: &Path, device: Id<Device>, now: i64) -> (Self, Vec<Damage>) {
        let (records, damage) = durable::read_all(data_dir);
        let mut clock = HybridClock::new(device);
        let mut drifts = Vec::new();
        for record in &records {
            if let Drift::Implausible { ahead_ms } = clock.observe(&record.stamp, now) {
                drifts.push(Drift::Implausible { ahead_ms });
            }
        }
        let flushed = durable::high_water(&records, device);
        (
            Instance {
                clock,
                records,
                drifts,
                flushed,
            },
            damage,
        )
    }

    /// Append everything this device has said since the last flush.
    ///
    /// **Only our own records.** Another device's records came from its file
    /// and belong in its file; copying them into ours would duplicate the log
    /// on every sync and make each machine's file grow by the whole library.
    ///
    /// The watermark makes this cheap: our own stamps strictly increase, so
    /// "ours, above the mark" is exactly "ours, not yet written" without
    /// re-reading anything.
    pub fn flush(&mut self, data_dir: &Path) -> Result<usize, LogError> {
        let device = self.clock.device();
        let pending: BTreeSet<Record> = self
            .records
            .iter()
            .filter(|r| r.stamp.device == device)
            .filter(|r| self.flushed.is_none_or(|mark| r.stamp > mark))
            .cloned()
            .collect();
        let written = durable::append(&durable::log_path(data_dir, device), &pending)?;
        if let Some(highest) = pending.iter().next_back() {
            self.flushed = Some(highest.stamp);
        }
        Ok(written)
    }

    pub fn merge(&mut self, now: i64, incoming: &BTreeSet<Record>) {
        for record in incoming {
            if let Drift::Implausible { ahead_ms } = self.clock.observe(&record.stamp, now) {
                self.drifts.push(Drift::Implausible { ahead_ms });
            }
        }
        self.records.extend(incoming.iter().cloned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Encoding;

    fn dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("gmuse-instance-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn recording(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// The point of the whole file: a rating survives the process that made it.
    #[test]
    fn what_was_flushed_comes_back_on_open() {
        let dir = dir("reopen");
        let d = device(1);
        let rec = recording(7);

        let mut one = Instance::new(d);
        one.rate(1_000, rec, Some(4));
        assert_eq!(one.flush(&dir).unwrap(), 1);

        let (two, damage) = Instance::open(&dir, d, 2_000);
        assert!(damage.is_empty());
        assert_eq!(
            two.catalogue().ratings.get(&rec).copied(),
            Some(4),
            "four stars, read back from disk"
        );
    }

    /// A flush with nothing new must write nothing. Otherwise every launch
    /// appends the whole library again and the log grows by its own size.
    #[test]
    fn flushing_twice_writes_once() {
        let dir = dir("incremental");
        let d = device(1);
        let mut one = Instance::new(d);
        one.rate(1_000, recording(7), Some(4));
        assert_eq!(one.flush(&dir).unwrap(), 1);
        assert_eq!(one.flush(&dir).unwrap(), 0, "nothing new to say");

        one.rate(2_000, recording(8), Some(3));
        assert_eq!(one.flush(&dir).unwrap(), 1, "and then only the new one");

        let (back, _) = crate::model::durable::read_all(&dir);
        assert_eq!(back.len(), 2, "two records on disk, not four");
    }

    /// **The guarantee `(device, seq)` never had.** A device that has been off
    /// opens, reads records stamped far in the future by another machine, and
    /// its next write must still sort *after* them — otherwise the fold decides
    /// the stale rating is the current one.
    #[test]
    fn a_write_after_a_read_sorts_after_what_was_read() {
        let dir = dir("observe");
        let (a, b) = (device(1), device(2));
        let rec = recording(7);

        // The other machine rated it, at a wall clock far ahead of ours.
        let mut theirs = Instance::new(a);
        theirs.rate(9_000_000, rec, Some(2));
        theirs.flush(&dir).unwrap();

        // We open with a clock that says it is much earlier, and re-rate.
        let (mut ours, _) = Instance::open(&dir, b, 1_000);
        ours.rate(1_000, rec, Some(5));

        assert_eq!(
            ours.catalogue().ratings.get(&rec).copied(),
            Some(5),
            "our later decision wins, despite our earlier clock"
        );
        let newest = ours.records().iter().next_back().expect("records");
        assert_eq!(newest.stamp.device, b, "and ours is the newest stamp");
    }

    /// Another device's records belong in another device's file. Writing them
    /// into ours duplicates the log on every sync, and each machine's file
    /// grows by the size of the whole library.
    #[test]
    fn a_flush_writes_only_this_devices_records() {
        let dir = dir("own-only");
        let (a, b) = (device(1), device(2));

        let mut theirs = Instance::new(a);
        theirs.rate(1_000, recording(7), Some(2));
        theirs.play(
            1_100,
            recording(7),
            Id::<Encoding>::from_uuid(uuid::Uuid::from_u128(3)),
        );

        let mut ours = Instance::new(b);
        ours.merge(2_000, theirs.records());
        ours.rate(2_000, recording(8), Some(5));
        assert_eq!(ours.flush(&dir).unwrap(), 1, "one record of ours");

        let (mine, _) = crate::model::durable::read(&crate::model::durable::log_path(&dir, b));
        assert_eq!(mine.len(), 1);
        assert!(
            mine.iter().all(|r| r.stamp.device == b),
            "nothing of theirs leaked into our file"
        );
    }

    /// A first run is an empty directory, not a failure.
    #[test]
    fn opening_nothing_gives_an_empty_instance() {
        let dir = dir("first-run");
        let (fresh, damage) = Instance::open(&dir, device(1), 1_000);
        assert!(fresh.records().is_empty());
        assert!(damage.is_empty());
        assert!(fresh.catalogue().recordings.is_empty());
    }

    /// Reopening must not re-write what is already there. The watermark is
    /// restored from disk, not reset to nothing.
    #[test]
    fn reopening_does_not_rewrite_the_log() {
        let dir = dir("watermark");
        let d = device(1);
        let mut one = Instance::new(d);
        one.rate(1_000, recording(7), Some(4));
        one.flush(&dir).unwrap();

        let (mut two, _) = Instance::open(&dir, d, 2_000);
        assert_eq!(two.flush(&dir).unwrap(), 0, "already on disk");

        let (back, _) = crate::model::durable::read_all(&dir);
        assert_eq!(back.len(), 1, "one record, written once");
    }
}
