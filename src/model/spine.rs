//! The containment spine — issue #72, `domain-model.md` §2.
//!
//! Work → Recording, and Release Group → Release → Medium → Track → Recording.
//! Six entities and one rule about how they point at each other.
//!
//! ## Associations are stored on the child
//!
//! `Medium.release`, `Track.medium`, `Track.recording`. **No entity holds a
//! collection of its children.** "The tracks of a medium" is derived, never
//! stored.
//!
//! This is not a weaker claim about containment — it is a stronger one. A
//! single scalar reference makes multi-parenting *unrepresentable* and
//! membership mandatory, where a parent-side `Vec` cheerfully represents a
//! track on two media, or on none. And an insert stays one new row rather than
//! a rewrite of the parent to be reconciled against every other rewrite of it.
//!
//! ## Order is a field
//!
//! `position`, never a container's sequence. An index cannot disagree with
//! itself, which sounds like a virtue and is the opposite: it means the index
//! can never reveal that the source data was wrong. An explicit position can be
//! checked for gaps and duplicates, and [`Spine::numbering_problems`] does.
//!
//! ## There is no such thing as "year"
//!
//! Four different dates get flattened into one column by tag formats, and the
//! reissue trap — a 2015 remaster carrying `date=2015` — is two of them
//! collapsed. They are kept apart here: `Recording.recorded`,
//! `Release.released`, `ReleaseGroup.first_released`.
//!
//! Each is a [`PartialDate`], because "1982" and "1982-03-01" are both things
//! the sources say and rounding the first to January would invent precision
//! nobody has.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::model::entity::Distribution;
use crate::model::id::{Id, Ref};

/// A date known to whatever precision the source knew it.
///
/// MusicBrainz and tags both give partial dates constantly. Storing a full date
/// would mean inventing a month and a day, and then being unable to tell an
/// invented January from a real one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PartialDate {
    pub year: i16,
    pub month: Option<u8>,
    pub day: Option<u8>,
}

impl PartialDate {
    pub const fn year(year: i16) -> Self {
        PartialDate {
            year,
            month: None,
            day: None,
        }
    }
}

/// Tempo, in hundredths of a beat per minute.
///
/// Not an `f32`. Two analyses of one track give 127.999 and 128.001, which are
/// the same tempo and are not equal — so a float makes `Recording` unable to
/// implement `Eq`, makes a BPM band boundary depend on rounding, and makes
/// "these two are the same tempo" a question with a tolerance in it. Hundredths
/// are finer than any analyser is honest about and compare exactly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Bpm(pub u32);

impl Bpm {
    pub const fn from_hundredths(n: u32) -> Self {
        Bpm(n)
    }

    /// Round to a band, for `bpm_band(4)` and its relatives. Integer maths, so
    /// a track on a boundary lands in the same band every time it is asked.
    pub const fn band(self, width: u32) -> u32 {
        (self.0 / 100) / width * width
    }

    pub fn as_f32(self) -> f32 {
        self.0 as f32 / 100.0
    }
}

/// A composition, independent of any performance of it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Work {
    pub id: Id<Work>,
    pub title: String,
}

/// One performance — **the grain**. What plays, and what history hangs off.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recording {
    pub id: Id<Recording>,
    pub title: String,
    /// **Optional, and that is not a defect.** 16% of this library has no
    /// MusicBrainz id and some music never will; a recording that realises no
    /// known work is an ordinary row.
    pub work: Option<Ref<Work>>,
    pub recorded: Option<PartialDate>,
    pub bpm: Option<Bpm>,
    pub length: Duration,
    /// A human overruling the canonical-encoding policy. Empty almost always.
    pub pinned: Option<Ref<Distribution>>,
}

/// The album across its editions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseGroup {
    pub id: Id<ReleaseGroup>,
    pub title: String,
    /// When the album first appeared — **not** when this pressing did. Sorting
    /// an artist's albums wants this one (#59).
    pub first_released: Option<PartialDate>,
    pub kind: ReleaseType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ReleaseType {
    #[default]
    Album,
    Single,
    Ep,
    Compilation,
    Live,
    Other,
}

impl ReleaseType {
    /// Whether a genre assigned here should be inherited by its recordings.
    ///
    /// A rock song on _80s Hits_ is reached through two release groups, and the
    /// naive union files it under the compilation's `Pop`. #74 prefers
    /// non-compilations for exactly this.
    pub const fn is_compilation(self) -> bool {
        matches!(self, ReleaseType::Compilation)
    }
}

/// One edition, with its catalogue number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Release {
    pub id: Id<Release>,
    pub group: Ref<ReleaseGroup>,
    pub title: String,
    pub released: Option<PartialDate>,
    pub catalogue_number: Option<String>,
}

/// A disc or a side.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Medium {
    pub id: Id<Medium>,
    pub release: Ref<Release>,
    pub position: u16,
    pub format: Option<String>,
}

/// A recording's slot on a medium.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub id: Id<Track>,
    pub medium: Ref<Medium>,
    pub recording: Ref<Recording>,
    pub position: u16,
    /// What the sleeve printed here, when it differs from the recording's own
    /// title. A compilation renaming a track is a fact about the compilation.
    pub title_on_release: Option<String>,
}

/// A recording's place in a mix, with when it enters.
///
/// Not a derivation: a bag of derivation rows drops the two facts a tracklist
/// is made of, and because the entries name **recordings** a mix can answer
/// which *version* of a track it played.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MixEntry {
    pub id: Id<MixEntry>,
    pub mix: Ref<Recording>,
    pub source: Ref<Recording>,
    pub position: u16,
    pub at: Option<Duration>,
}

/// Compositional acts — a new work made out of an old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkDerivationKind {
    Arrangement,
    Medley,
    Quotation,
}

/// A version of a version. **A cover is not here**: it realises the existing
/// work, as a performance does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecordingDerivationKind {
    Remix,
    Edit,
    Dub,
    Mashup,
    Sample,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkDerivation {
    pub from: Ref<Work>,
    pub to: Ref<Work>,
    pub kind: WorkDerivationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordingDerivation {
    pub from: Ref<Recording>,
    pub to: Ref<Recording>,
    pub kind: RecordingDerivationKind,
}

/// The spine, held flat.
///
/// Flat because the containment is on the children: there is no tree to walk
/// down, only rows that name their parent. Every "what is inside this" question
/// is therefore a **derived** answer, computed here rather than stored — which
/// is what stops it from disagreeing with the rows it summarises.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Spine {
    pub works: BTreeMap<Id<Work>, Work>,
    pub recordings: BTreeMap<Id<Recording>, Recording>,
    pub groups: BTreeMap<Id<ReleaseGroup>, ReleaseGroup>,
    pub releases: BTreeMap<Id<Release>, Release>,
    pub media: BTreeMap<Id<Medium>, Medium>,
    pub tracks: BTreeMap<Id<Track>, Track>,
}

impl Spine {
    /// The editions of an album, in first-released order.
    pub fn releases_of(&self, group: Id<ReleaseGroup>) -> Vec<&Release> {
        let mut found: Vec<_> = self
            .releases
            .values()
            .filter(|r| r.group.id() == group)
            .collect();
        found.sort_by(|a, b| a.released.cmp(&b.released).then(a.id.cmp(&b.id)));
        found
    }

    /// The discs of an edition, in position order.
    pub fn media_of(&self, release: Id<Release>) -> Vec<&Medium> {
        let mut found: Vec<_> = self
            .media
            .values()
            .filter(|m| m.release.id() == release)
            .collect();
        found.sort_by_key(|m| (m.position, m.id));
        found
    }

    /// The tracks of a disc, in position order.
    pub fn tracks_of(&self, medium: Id<Medium>) -> Vec<&Track> {
        let mut found: Vec<_> = self
            .tracks
            .values()
            .filter(|t| t.medium.id() == medium)
            .collect();
        found.sort_by_key(|t| (t.position, t.id));
        found
    }

    /// Every place a recording appears — which is why a recording held on both
    /// an album and an anthology is one row and two appearances, and why
    /// playing it from either counts once.
    pub fn tracks_of_recording(&self, recording: Id<Recording>) -> Vec<&Track> {
        let mut found: Vec<_> = self
            .tracks
            .values()
            .filter(|t| t.recording.id() == recording)
            .collect();
        found.sort_by_key(|t| t.id);
        found
    }

    /// The recordings realising a work — a composition's performances, and a
    /// song's covers, which are the same relation.
    pub fn recordings_of(&self, work: Id<Work>) -> Vec<&Recording> {
        let mut found: Vec<_> = self
            .recordings
            .values()
            .filter(|r| r.work.map(|w| w.id()) == Some(work))
            .collect();
        found.sort_by_key(|r| r.id);
        found
    }

    /// Numbering that cannot be right: a gap, a repeat, or a zero.
    ///
    /// The whole reason position is a field. An index position is always
    /// "correct" by construction and so can never say that the source data was
    /// wrong; a stored one can be checked, and this is the check.
    pub fn numbering_problems(&self) -> Vec<NumberingProblem> {
        let mut problems = Vec::new();
        for medium in self.media.keys() {
            let positions: Vec<u16> = self.tracks_of(*medium).iter().map(|t| t.position).collect();
            for (i, p) in positions.iter().enumerate() {
                let expected = i as u16 + 1;
                if *p == 0 {
                    problems.push(NumberingProblem::Zero { medium: *medium });
                } else if i > 0 && *p == positions[i - 1] {
                    problems.push(NumberingProblem::Repeated {
                        medium: *medium,
                        position: *p,
                    });
                } else if *p != expected {
                    problems.push(NumberingProblem::Gap {
                        medium: *medium,
                        expected,
                        found: *p,
                    });
                }
            }
        }
        problems
    }
}

/// Something wrong with a medium's track numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberingProblem {
    Gap {
        medium: Id<Medium>,
        expected: u16,
        found: u16,
    },
    Repeated {
        medium: Id<Medium>,
        position: u16,
    },
    Zero {
        medium: Id<Medium>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spine_with_an_album_and_an_anthology() -> (Spine, Id<Recording>, Id<Medium>, Id<Medium>) {
        let mut s = Spine::default();

        let big_science = Id::<Recording>::derived(b"big-science");
        s.recordings.insert(
            big_science,
            Recording {
                id: big_science,
                title: "Big Science".into(),
                work: None,
                recorded: None,
                bpm: None,
                length: Duration::from_secs(400),
                pinned: None,
            },
        );

        let mut media = Vec::new();
        for (n, (title, year)) in [("Big Science", 1982), ("Talk Normal: Anthology", 2000)]
            .into_iter()
            .enumerate()
        {
            let g = Id::<ReleaseGroup>::derived(title.as_bytes());
            s.groups.insert(
                g,
                ReleaseGroup {
                    id: g,
                    title: title.into(),
                    first_released: Some(PartialDate::year(year)),
                    kind: if n == 1 {
                        ReleaseType::Compilation
                    } else {
                        ReleaseType::Album
                    },
                },
            );
            let r = Id::<Release>::derived(title.as_bytes());
            s.releases.insert(
                r,
                Release {
                    id: r,
                    group: Ref::to(g),
                    title: title.into(),
                    released: Some(PartialDate::year(year)),
                    catalogue_number: None,
                },
            );
            let m = Id::<Medium>::derived(title.as_bytes());
            s.media.insert(
                m,
                Medium {
                    id: m,
                    release: Ref::to(r),
                    position: 1,
                    format: Some("CD".into()),
                },
            );
            let t = Id::<Track>::derived(format!("{title}-track").as_bytes());
            s.tracks.insert(
                t,
                Track {
                    id: t,
                    medium: Ref::to(m),
                    recording: Ref::to(big_science),
                    position: if n == 0 { 2 } else { 3 },
                    title_on_release: None,
                },
            );
            media.push(m);
        }
        (s, big_science, media[0], media[1])
    }

    /// One recording, two releases — the 104 recordings this library already
    /// holds twice, and the reason they are one row rather than two tracks.
    #[test]
    fn a_recording_appears_on_every_release_that_holds_it() {
        let (s, big_science, album, anthology) = spine_with_an_album_and_an_anthology();

        let appearances = s.tracks_of_recording(big_science);
        assert_eq!(appearances.len(), 2, "on the album and on the anthology");
        assert_eq!(s.tracks_of(album)[0].position, 2);
        assert_eq!(
            s.tracks_of(anthology)[0].position,
            3,
            "the same performance, numbered differently on each"
        );
    }

    /// Containment is read downward and stored upward, and the two agree
    /// because only one of them exists.
    #[test]
    fn what_is_inside_something_is_derived_not_stored() {
        let (s, _, album, _) = spine_with_an_album_and_an_anthology();
        let release = s.media[&album].release.id();
        let group = s.releases[&release].group.id();

        assert_eq!(s.media_of(release).len(), 1);
        assert_eq!(s.releases_of(group).len(), 1);
        assert_eq!(s.tracks_of(album).len(), 1);
    }

    /// A cover and a performance are the same relation: one work, many
    /// recordings. All three _Buckets of Rain_ files here carry
    /// `composer: Bob Dylan`.
    #[test]
    fn a_work_gathers_its_performances_and_its_covers_alike() {
        let mut s = Spine::default();
        let work = Id::<Work>::derived(b"buckets-of-rain");
        s.works.insert(
            work,
            Work {
                id: work,
                title: "Buckets of Rain".into(),
            },
        );
        for who in ["Bob Dylan", "Neko Case", "David Gray"] {
            let id = Id::<Recording>::derived(who.as_bytes());
            s.recordings.insert(
                id,
                Recording {
                    id,
                    title: "Buckets of Rain".into(),
                    work: Some(Ref::to(work)),
                    recorded: None,
                    bpm: None,
                    length: Duration::from_secs(200),
                    pinned: None,
                },
            );
        }
        assert_eq!(
            s.recordings_of(work).len(),
            3,
            "not a derived work in sight"
        );
    }

    /// **Why position is a field.** An index cannot disagree with itself, so it
    /// can never say the source was wrong. A stored one can be checked.
    #[test]
    fn bad_track_numbering_is_detectable() {
        let (mut s, rec, album, _) = spine_with_an_album_and_an_anthology();
        assert_eq!(
            s.numbering_problems().len(),
            2,
            "both fixtures start at 2 and 3, which is already a gap"
        );

        // A duplicate is the case an index physically cannot represent.
        let dup = Id::<Track>::derived(b"dup");
        s.tracks.insert(
            dup,
            Track {
                id: dup,
                medium: Ref::to(album),
                recording: Ref::to(rec),
                position: 2,
                title_on_release: None,
            },
        );
        assert!(
            s.numbering_problems()
                .iter()
                .any(|p| matches!(p, NumberingProblem::Repeated { .. })),
            "two tracks at position 2 is a thing to be told about"
        );
    }

    /// The reissue trap: a 2015 remaster carrying `date=2015` is two levels
    /// collapsed into one column. Kept apart, "album order by release date"
    /// (#59) reads the group's date and gets 1982.
    #[test]
    fn the_four_dates_do_not_collapse_into_one() {
        let (mut s, _, album, _) = spine_with_an_album_and_an_anthology();
        let release = s.media[&album].release.id();
        let group = s.releases[&release].group.id();

        // A 2015 reissue of the 1982 album.
        let reissue = Id::<Release>::derived(b"big-science-2015");
        s.releases.insert(
            reissue,
            Release {
                id: reissue,
                group: Ref::to(group),
                title: "Big Science".into(),
                released: Some(PartialDate::year(2015)),
                catalogue_number: Some("NON-2015".into()),
            },
        );

        assert_eq!(
            s.groups[&group].first_released,
            Some(PartialDate::year(1982)),
            "the album is from 1982 however many times it is pressed"
        );
        let editions = s.releases_of(group);
        assert_eq!(editions.len(), 2);
        assert_eq!(editions[0].released, Some(PartialDate::year(1982)));
        assert_eq!(editions[1].released, Some(PartialDate::year(2015)));
    }

    /// A partial date stays partial. Rounding 1982 to the first of January
    /// invents a precision nobody has, and then cannot be told from a real one.
    #[test]
    fn a_year_is_not_a_january() {
        let year = PartialDate::year(1982);
        assert_eq!((year.month, year.day), (None, None));
        assert!(
            year < PartialDate {
                year: 1982,
                month: Some(1),
                day: None
            },
            "known-to-the-month sorts after known-only-to-the-year"
        );
    }

    /// A tempo compares exactly, so a band boundary does not move under
    /// rounding.
    #[test]
    fn tempo_is_exact_and_bands_are_stable() {
        let a = Bpm::from_hundredths(12_800);
        let b = Bpm::from_hundredths(12_799);
        assert_ne!(a, b, "and neither is 'approximately' the other");
        assert_eq!(a.band(4), 128, "128 lands in the 128 band");
        assert_eq!(b.band(4), 124, "127.99 lands in the one below, every time");
        assert_eq!(Bpm::from_hundredths(13_000).band(4), 128);
        assert!((a.as_f32() - 128.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_spine_round_trips_through_storage() {
        let (s, _, _, _) = spine_with_an_album_and_an_anthology();
        let bytes = postcard::to_allocvec(&s).unwrap();
        let back: Spine = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.recordings, s.recordings);
        assert_eq!(back.tracks, s.tracks);
    }
}
