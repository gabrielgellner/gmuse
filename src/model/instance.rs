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
use crate::model::basis::Basis;
use crate::model::clock::{Drift, HybridClock, Stamp};
use crate::model::durable::{self, Damage, LogError};
use crate::model::encoding::Format;
use crate::model::entity::{
    Artist, Device, Encoding, Genre, Hierarchy, Medium, Qualifier, Recording, Release,
    ReleaseGroup, Track,
};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::log::{Catalogue, Fact, Record, compact, fold};
use crate::model::reference::{AliasKind, Cataloguable, Source};
use crate::model::vocabulary::GenreOn;

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
    /// The artist's MusicBrainz id, where the tags carry one — 92.7% of this
    /// library. Without it the artist is minted from the credit *string*, and
    /// a guest credit makes a new artist: `2Pac feat. Nate Dogg` is not `2Pac`
    /// to a hash (#89 §5).
    pub artist_mbid: Option<uuid::Uuid>,
    /// **What names this performance**, if anything does — the identity half
    /// of a scan, kept apart from the descriptive half below.
    ///
    /// A `Basis` rather than a MusicBrainz id, so a pass that harvests a
    /// Beatport track id or a video id can fill this in without `ingest`
    /// learning what either of those is. `None` falls to the bytes, which is
    /// the rung that a re-encode strands (#89).
    pub identity: Option<crate::model::basis::Basis>,
    /// The album this sits on, as the tags name it.
    pub album: Option<String>,
    /// The album artist's MusicBrainz id. Distinct from `artist_mbid`: on a
    /// compilation the track artist and the album artist are different people.
    pub album_artist_mbid: Option<uuid::Uuid>,
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
    /// The **release** id, which Picard writes beside the group's and which
    /// names the pressing rather than the album. Distinct on purpose: two
    /// files can share a group and be different pressings, and that is the
    /// whole reason the release level exists.
    pub mb_release: Option<uuid::Uuid>,
    /// How long it plays, in milliseconds.
    ///
    /// A fact about the **bytes**, not about the performance: an edit and its
    /// original are one recording to MusicBrainz and different lengths on
    /// disk. It rides with the encoding for that reason, and because a player
    /// deciding whether a listen counted needs it before anything else.
    pub length_ms: u32,
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
            let artist = Id::<Artist>::of_artist(s.artist_mbid, &s.artist);
            let encoding = Id::<Encoding>::of_audio(&s.audio);
            // The best rung this file reaches. Nothing names the performance,
            // so it falls to the bytes — which is convergent, and is the case
            // a re-encode strands. Random minting is not the alternative: two
            // devices reading one disk would disagree about a song neither has
            // any doubt about, and the same device would disagree with itself
            // across a rescan.
            let basis = s.identity.clone().unwrap_or(Basis::Audio(s.audio));
            let minted = Id::<Recording>::of_basis(&basis);

            // **A binding someone made wins over the one we would mint.**
            //
            // This used to be described as stopping a rescan from doubling the
            // library, and it is not that any more — `of_basis` is
            // deterministic, so a rescan mints the same id it minted last
            // time. What it does now is narrower and more important: if a
            // relink pointed this encoding at a different recording, a scan
            // must not quietly mint over the decision (#88).
            let bound = known.encodings.get(&encoding).and_then(|d| d.recording);
            let recording = bound.unwrap_or(minted);

            // Only what we actually minted carries a basis. Claiming that a
            // relinked recording was derived from these bytes would be false,
            // and it is the sort of false that a later fingerprint pass would
            // act on.
            if bound.is_none() {
                self.append(
                    now,
                    Fact::RecordingIdentified {
                        recording,
                        basis: basis.clone(),
                    },
                );
            }

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
                // The album's credit, by the same ladder. Its MusicBrainz id
                // is the *album artist's*, which is why it is not
                // `artist_mbid` — on a compilation those are different people.
                let group_artist = Id::<Artist>::of_artist(s.album_artist_mbid, credited);
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
                // **The three levels a file actually sits in.** A scan sees
                // one pressing — the one on disk — so the release falls back
                // to the group rather than inventing a second; the disc
                // number, which used to ride on the placement, is now the
                // medium it was standing in for.
                let release = Id::<Release>::of_release(s.mb_release, group);
                self.append(
                    now,
                    Fact::ReleaseCatalogued {
                        release,
                        group,
                        title: album.clone(),
                        released: None,
                    },
                );
                let disc = s.disc.unwrap_or(1);
                let medium = Id::<Medium>::of_medium(release, disc);
                self.append(
                    now,
                    Fact::MediumCatalogued {
                        medium,
                        release,
                        position: disc,
                        format: None,
                    },
                );
                self.append(
                    now,
                    Fact::TrackPlaced {
                        track: Id::<Track>::of_track(medium, recording),
                        medium,
                        recording,
                        position: s.track.unwrap_or(0),
                        title_on_release: None,
                    },
                );
            }

            self.append(
                now,
                Fact::EncodingDescribed {
                    encoding,
                    audio: s.audio,
                    format: s.format,
                    bitrate: s.bitrate,
                    length_ms: s.length_ms,
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

    pub fn assign_genre(
        &mut self,
        now: i64,
        subject: GenreOn,
        name: &str,
        basis: Source,
        confidence: Confidence,
    ) {
        // **The concept before the assignment.** An id that no fact resolves
        // is an id nobody can render, so the vocabulary entry is asserted
        // alongside — idempotently, since both ids derive from the name and a
        // repeat is the same record.
        let genre = Id::<Genre>::of_name(name);
        self.append(
            now,
            Fact::GenreCatalogued {
                genre,
                name: name.trim().to_string(),
            },
        );
        self.append(
            now,
            Fact::GenreAssigned {
                subject,
                genre,
                basis,
                confidence,
            },
        );
    }

    /// Say which version of a song a recording is.
    ///
    /// Refuses a grouping concept: `mix-variant` is a place in the tree, not a
    /// claim about a song, and accepting it would fold to an assignment nobody
    /// can see — indistinguishable from one nobody made.
    pub fn assign_qualifier(
        &mut self,
        now: i64,
        recording: Id<Recording>,
        label: &str,
        basis: Source,
        confidence: Confidence,
    ) -> bool {
        if !crate::model::qualifier::assignable(label) {
            return false;
        }
        let concept = Id::<Qualifier>::of_label(label);
        self.append(
            now,
            Fact::QualifierCatalogued {
                concept,
                label: label.trim().to_string(),
            },
        );
        self.append(
            now,
            Fact::QualifierAssigned {
                recording,
                concept,
                basis,
                confidence,
            },
        );
        true
    }

    /// Rename a concept, keeping the name it had.
    ///
    /// **A rename does not re-mint.** The id came from the old label, and
    /// every assignment points at it; re-minting on rename would rewrite every
    /// one of them, and the whole reason ids are derived is that two devices
    /// can define a concept independently and land on the same row.
    ///
    /// The cost is a *false split*: a device that meets the new label fresh
    /// mints a different id, and now one concept has two. That is accepted
    /// rather than solved, because it is the repairable error — a split is one
    /// merge from repaired, and a false merge is permanent. The alias is what
    /// makes the repair findable: the merge review sees two concepts sharing a
    /// name and proposes, and a redirect resolves both ids to one for good.
    ///
    /// `Retired` rather than `SearchHint`, because the name did not stop being
    /// this concept's — it stopped being the one it is called by.
    pub fn rename_genre(&mut self, now: i64, genre: Id<Genre>, was: &str, now_called: &str) {
        self.append(
            now,
            Fact::GenreCatalogued {
                genre,
                name: now_called.trim().to_string(),
            },
        );
        self.append(
            now,
            Fact::AliasAsserted {
                subject: Cataloguable::Genre(genre),
                value: was.trim().to_string(),
                kind: AliasKind::Retired,
            },
        );
    }

    /// Decide a genre does not apply. Shadows rather than deletes, and
    /// silences every asserter — see [`Instance::retract_qualifier`].
    pub fn retract_genre(&mut self, now: i64, subject: GenreOn, name: &str) {
        self.append(
            now,
            Fact::GenreRetracted {
                subject,
                genre: Id::<Genre>::of_name(name),
            },
        );
    }

    /// Decide a qualifier does not apply. Shadows rather than deletes.
    pub fn retract_qualifier(&mut self, now: i64, recording: Id<Recording>, label: &str) {
        self.append(
            now,
            Fact::QualifierRetracted {
                recording,
                concept: Id::<Qualifier>::of_label(label),
            },
        );
    }

    /// Write the version vocabulary into the log, so its ids resolve.
    ///
    /// Idempotent: every id derives from its label, so a second call appends
    /// records the set already holds.
    pub fn seed_qualifiers(&mut self, now: i64) {
        for (label, parent, _) in crate::model::qualifier::SEED {
            let concept = Id::<Qualifier>::of_label(label);
            self.append(
                now,
                Fact::QualifierCatalogued {
                    concept,
                    label: (*label).to_string(),
                },
            );
            if let Some(p) = parent {
                self.append(
                    now,
                    Fact::QualifierNarrowed {
                        parent: Id::<Qualifier>::of_label(p),
                        child: concept,
                    },
                );
            }
        }
    }

    /// Say that one concept is narrower than another, inside one hierarchy.
    pub fn narrows(&mut self, now: i64, hierarchy: Id<Hierarchy>, parent: &str, child: &str) {
        self.append(
            now,
            Fact::HierarchyNarrowed {
                hierarchy,
                parent: Id::<Genre>::of_name(parent),
                child: Id::<Genre>::of_name(child),
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
            Fact::EncodingMeasured {
                encoding,
                cutoff_hz,
                analyser,
            },
        );
    }

    pub fn rate(&mut self, now: i64, recording: Id<Recording>, stars: Option<u8>) {
        self.append(now, Fact::RecordingRated { recording, stars });
    }

    /// Record when a recording entered the library.
    ///
    /// Idempotent by construction: the fold takes the minimum, so saying it
    /// twice, or saying it later than another device did, changes nothing.
    pub fn acquire(&mut self, now: i64, recording: Id<Recording>, at: i64) {
        self.append(now, Fact::RecordingAcquired { recording, at });
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
            Fact::RecordingPlayed {
                recording,
                encoding,
            },
        );
    }

    pub fn play(&mut self, now: i64, recording: Id<Recording>, encoding: Id<Encoding>) {
        self.append(
            now,
            Fact::RecordingPlayed {
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

#[cfg(test)]
mod containment_tests {
    //! **The three levels a file actually sits in.**
    //!
    //! A recording does not sit on an album. It sits on a disc, of a pressing,
    //! of an album — and the model spent its first year with the middle two
    //! missing, `TrackPlaced` naming a release *group* and carrying a disc
    //! number that was standing in for the medium nobody had modelled.

    use super::*;
    use crate::model::entity::{Release, Track};

    fn scan(disc: u16, track: u16, mb_release: Option<u128>) -> Scanned {
        Scanned {
            audio: crate::audio_id::AudioId {
                scheme: 1,
                hash: u128::from(disc) << 64 | u128::from(track),
            },
            title: format!("Track {track}"),
            artist: "Bodzin".into(),
            artist_mbid: None,
            identity: Some(crate::model::basis::Basis::MusicBrainz(
                uuid::Uuid::from_u128(u128::from(disc) << 32 | u128::from(track)),
            )),
            album: Some("Luna".into()),
            album_artist: Some("Bodzin".into()),
            album_artist_mbid: None,
            disc: Some(disc),
            track: Some(track),
            mb_release_group: Some(uuid::Uuid::from_u128(0x9e11)),
            mb_release: mb_release.map(uuid::Uuid::from_u128),
            length_ms: 200_000,
            format: Some(crate::model::encoding::Format::Mp3),
            bitrate: 320,
        }
    }

    /// One file, three rows, and each names the one above it.
    ///
    /// Before this the placement named a group directly, so "which pressing
    /// did this come from" had no row that could answer and the disc number
    /// was the only trace of the medium.
    #[test]
    fn a_scan_places_a_recording_on_a_medium_of_a_release() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 3, Some(0x5e1e))]);
        let cat = i.catalogue();

        let (release, r) = cat.releases.iter().next().expect("a pressing");
        assert_eq!(cat.groups.len(), 1);
        assert!(
            cat.groups.contains_key(&r.group),
            "the release lost its album"
        );

        let (medium, m) = cat.media.iter().next().expect("a disc");
        assert_eq!(m.release, *release);
        assert_eq!(m.position, 1, "the disc number is the medium's position");

        let (_, placed) = cat.tracks.iter().next().expect("a slot");
        assert_eq!(placed.medium, *medium);
        assert_eq!(placed.position, 3, "the track number is the slot's");
    }

    /// **A two-disc album is two media, not one.** The shape the old fact
    /// could not hold: both discs collapsed onto the group, and the only thing
    /// telling them apart was a number on the placement.
    #[test]
    fn each_disc_of_a_release_is_its_own_medium() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 1, Some(0x5e1e)), scan(2, 1, Some(0x5e1e))]);
        let cat = i.catalogue();

        assert_eq!(cat.releases.len(), 1, "one pressing, two discs");
        assert_eq!(cat.media.len(), 2);
        assert_eq!(
            cat.media.values().map(|m| m.position).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(cat.tracks.len(), 2, "two slots, one on each disc");
    }

    /// **A release the tags name and one they do not are different rows**, and
    /// the fallback is the group rather than an invented second pressing.
    ///
    /// The old projection minted a release for every group unconditionally, so
    /// a library with MusicBrainz release ids in its tags threw them away and
    /// used a hash of the group instead.
    #[test]
    fn a_tagged_pressing_is_not_the_untagged_fallback() {
        let group = Id::<ReleaseGroup>::of_musicbrainz(uuid::Uuid::from_u128(0x9e11));
        let mbid = uuid::Uuid::from_u128(0x5e1e);
        let tagged = Id::<Release>::of_release(Some(mbid), group);
        let fallback = Id::<Release>::of_release(None, group);
        assert_ne!(tagged, fallback);

        // On the MusicBrainz rung the group is not part of the identity: the
        // release id names the pressing outright, and two files that disagree
        // about which album it belongs to still hold the same pressing.
        let elsewhere = Id::<ReleaseGroup>::of_musicbrainz(uuid::Uuid::from_u128(0xffff));
        assert_eq!(tagged, Id::<Release>::of_release(Some(mbid), elsewhere));
        assert_eq!(tagged, Id::<Release>::of_musicbrainz(mbid));

        // And each is convergent: the point is that two devices agree without
        // meeting, whichever rung they are on.
        assert_eq!(
            fallback,
            Id::<Release>::of_release(None, group),
            "the fallback has to be derived, or two untagged copies split"
        );

        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 1, None)]);
        assert!(i.catalogue().releases.contains_key(&fallback));
    }

    /// **A corrected track number does not mint a new slot.**
    ///
    /// The reason `of_track` keys on the recording. Keyed on position, fixing
    /// a tracklist would mint a track for every row that shifted and strand
    /// whatever hung off the old ones — a false split with nothing to merge it
    /// back, where this way the number is just an attribute that moved.
    #[test]
    fn renumbering_a_track_moves_it_rather_than_replacing_it() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 3, Some(0x5e1e))]);
        let before: Vec<Id<Track>> = i.catalogue().tracks.keys().copied().collect();

        // The same file, rescanned after somebody fixed the tag.
        let mut fixed = scan(1, 3, Some(0x5e1e));
        fixed.track = Some(4);
        i.ingest(200, &[fixed]);

        let cat = i.catalogue();
        assert_eq!(cat.tracks.keys().copied().collect::<Vec<_>>(), before);
        assert_eq!(cat.tracks.values().next().unwrap().position, 4);
    }

    /// **Two recordings on one disc are two slots.** Obvious, and the thing a
    /// track id keyed on the medium alone would silently get wrong — every
    /// album folding to a single track that the last file scanned won.
    #[test]
    fn each_recording_on_a_medium_is_its_own_slot() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 1, Some(0x5e1e)), scan(1, 2, Some(0x5e1e))]);
        let cat = i.catalogue();
        assert_eq!(cat.media.len(), 1, "one disc");
        assert_eq!(cat.tracks.len(), 2, "two slots on it");
        assert_eq!(
            cat.tracks
                .values()
                .map(|p| p.recording)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2,
            "both slots name the same recording"
        );
    }

    /// Two devices reading the same tags agree on all three levels, with no
    /// conversation — the bet the whole identity scheme makes, at the level
    /// this issue added.
    #[test]
    fn two_devices_agree_on_the_pressing_and_the_disc() {
        let files = [scan(1, 1, Some(0x5e1e)), scan(2, 4, Some(0x5e1e))];
        let mut a = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        let mut b = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(2)));
        a.ingest(100, &files);
        b.ingest(999, &files);
        let (x, y) = (a.catalogue(), b.catalogue());
        assert_eq!(x.releases, y.releases);
        assert_eq!(x.media, y.media);
        assert_eq!(x.tracks, y.tracks);
    }
}
