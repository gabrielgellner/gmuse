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

use crate::model::entity::Encoding;
use crate::model::entity::Label;
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

/// The first four-digit run in a messy date, if there is one.
fn four_digit_year(text: &str) -> Option<i16> {
    let bytes = text.as_bytes();
    let mut run = 0usize;
    for (at, b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            run += 1;
        } else {
            if run == 4 {
                return text[at - 4..at].parse().ok();
            }
            run = 0;
        }
    }
    (run == 4)
        .then(|| text[text.len() - 4..].parse().ok())
        .flatten()
}

impl PartialDate {
    /// Read a date as tags write it: `1995`, `1996-03`, `1996-03-16`.
    ///
    /// **Partial on purpose.** A sleeve that says 1995 is not claiming the
    /// first of January, and turning it into one would make a date sort ahead
    /// of every dated day in the same year while looking like a fact somebody
    /// asserted. Anything after the year that will not parse is dropped rather
    /// than failing the whole date — a malformed month is worth less than the
    /// year beside it.
    /// **And a messy date still gives up its year** (#86). Sleeves carry
    /// `(1997)` and `recorded 2016 live`, and the store's own reader took the
    /// first four-digit run out of both. That reader is gone and this is the
    /// only one left, so it has to be at least as forgiving or those files
    /// silently lose their year.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split(['-', '/', '.']);
        let head = parts.next()?.trim();
        let year = match head.parse::<i16>() {
            // **Four digits, or it is not a year.** `97` parses perfectly well
            // as ninety-seven and means nothing anybody can place.
            Ok(y) if head.len() == 4 => y,
            // Otherwise the first run of exactly four digits, if there is
            // one — which is what pulls a year out of `(1997)`.
            _ => four_digit_year(text)?,
        };
        let month = parts
            .next()
            .and_then(|m| m.trim().parse::<u8>().ok())
            .filter(|m| (1..=12).contains(m));
        let day = month
            .and(parts.next())
            .and_then(|d| d.trim().parse::<u8>().ok())
            .filter(|d| (1..=31).contains(d));
        Some(PartialDate { year, month, day })
    }

    #[allow(
        dead_code,
        reason = "the store's own date reader was its caller (#86); fixtures use it, and it is the honest constructor for a sleeve that names only a year"
    )]
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

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — BPM is modelled and tested; #50's harmonic nearness is where it gets a caller"
)]
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
    /// **Optional, and that is not a defect.** 7.3% of this library has no
    /// MusicBrainz id and some music never will; a recording that realises no
    /// known work is an ordinary row.
    pub work: Option<Ref<Work>>,
    pub recorded: Option<PartialDate>,
    pub bpm: Option<Bpm>,
    pub length: Duration,
    /// A human overruling the canonical-encoding policy. Empty almost always.
    pub pinned: Option<Ref<Encoding>>,
}

/// The album across its editions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseGroup {
    pub id: Id<ReleaseGroup>,
    pub title: String,
    /// When the album first appeared — **not** when this pressing did. Sorting
    /// an artist's albums wants this one (#59).
    pub first_released: Option<PartialDate>,
    /// Album, single, EP — the one answer.
    pub kind: ReleaseType,
    /// And whatever else it also is. Usually empty.
    pub secondary: std::collections::BTreeSet<ReleaseSecondary>,
}

impl ReleaseGroup {
    /// Whether a genre assigned here should be inherited by its recordings.
    ///
    /// A rock song on _80s Hits_ is reached through two release groups, and the
    /// naive union files it under the compilation's `Pop`. #74 prefers
    /// non-compilations for exactly this.
    ///
    /// **Asked of the secondary set**, because that is where MusicBrainz puts
    /// it: an `album/compilation` is both, and a flat enum that had to choose
    /// would answer "album" and lose the rule (#133).
    #[allow(
        dead_code,
        reason = "the #74 rule that a compilation's genre loses to an album's is not wired up"
    )]
    pub fn is_compilation(&self) -> bool {
        self.secondary.contains(&ReleaseSecondary::Compilation)
    }
}

/// **What kind of release this is** — MusicBrainz's *primary* type (#133).
///
/// One of these, always. It answers "is this an album or a single", which is a
/// question with one answer.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum ReleaseType {
    /// **The default, and it is a default rather than a claim.** Nothing said
    /// otherwise; almost everything is an album, so this is the cheapest wrong
    /// answer available and the one a missing tag deserves.
    #[default]
    Album,
    Single,
    Ep,
    Broadcast,
    Other,
}

/// **The other things a release also is** — MusicBrainz's *secondary* types.
///
/// Several at once, which is why this is a set and why it is not folded into
/// [`ReleaseType`]. The tags in this library carry `album/compilation/dj-mix`
/// on real files: one release that is an album, a compilation and a DJ mix
/// together. A single flat enum has to pick one of the three and throw the
/// rest away.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ReleaseSecondary {
    Compilation,
    Soundtrack,
    Spokenword,
    Interview,
    Audiobook,
    AudioDrama,
    Live,
    Remix,
    DjMix,
    Mixtape,
    Demo,
    FieldRecording,
}

impl ReleaseType {
    /// How it is written — in the query language, and as MusicBrainz spells it.
    ///
    /// **One spelling, used for both**, because a second list is a second
    /// thing to keep in step. `kind="single"` matches what the tag says.
    pub const fn written(self) -> &'static str {
        match self {
            ReleaseType::Album => "album",
            ReleaseType::Single => "single",
            ReleaseType::Ep => "ep",
            ReleaseType::Broadcast => "broadcast",
            ReleaseType::Other => "other",
        }
    }

    /// Read one back, from a tag or from the `:` line. Case-insensitive,
    /// because a tag is written by whatever wrote it.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase();
        [
            ReleaseType::Album,
            ReleaseType::Single,
            ReleaseType::Ep,
            ReleaseType::Broadcast,
            ReleaseType::Other,
        ]
        .into_iter()
        .find(|k| k.written() == t)
    }
}

impl ReleaseSecondary {
    pub const fn written(self) -> &'static str {
        match self {
            ReleaseSecondary::Compilation => "compilation",
            ReleaseSecondary::Soundtrack => "soundtrack",
            ReleaseSecondary::Spokenword => "spokenword",
            ReleaseSecondary::Interview => "interview",
            ReleaseSecondary::Audiobook => "audiobook",
            ReleaseSecondary::AudioDrama => "audio drama",
            ReleaseSecondary::Live => "live",
            ReleaseSecondary::Remix => "remix",
            ReleaseSecondary::DjMix => "dj-mix",
            ReleaseSecondary::Mixtape => "mixtape",
            ReleaseSecondary::Demo => "demo",
            ReleaseSecondary::FieldRecording => "field recording",
        }
    }

    /// Every one, so a list is never typed out twice.
    pub const ALL: [ReleaseSecondary; 12] = [
        ReleaseSecondary::Compilation,
        ReleaseSecondary::Soundtrack,
        ReleaseSecondary::Spokenword,
        ReleaseSecondary::Interview,
        ReleaseSecondary::Audiobook,
        ReleaseSecondary::AudioDrama,
        ReleaseSecondary::Live,
        ReleaseSecondary::Remix,
        ReleaseSecondary::DjMix,
        ReleaseSecondary::Mixtape,
        ReleaseSecondary::Demo,
        ReleaseSecondary::FieldRecording,
    ];

    /// Read one back. `mixtape/street` is MusicBrainz's own spelling of the
    /// type this calls `mixtape`, so both find it.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase();
        let t = if t == "mixtape/street" { "mixtape" } else { &t };
        Self::ALL.into_iter().find(|s| s.written() == t)
    }
}

/// One edition, with its catalogue number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Release {
    pub id: Id<Release>,
    pub group: Ref<ReleaseGroup>,
    pub title: String,
    pub released: Option<PartialDate>,
    /// Who issued it. **On the pressing rather than the album**, because that
    /// is what a reissue changes: the record is the same and the imprint is
    /// not.
    pub label: Option<Ref<Label>>,
    pub catalogue_number: Option<String>,
    /// How many discs this pressing has, where anything said — #134.
    pub media: Option<u16>,
}

/// A disc or a side.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Medium {
    pub id: Id<Medium>,
    pub release: Ref<Release>,
    pub position: u16,
    pub format: Option<String>,
    /// How many tracks this disc has, where anything said — #134.
    pub tracks: Option<u16>,
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

#[allow(
    dead_code,
    reason = "the mix model, proposed in domain-model.md; nothing mints one"
)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum WorkDerivationKind {
    Arrangement,
    Medley,
    Quotation,
}

/// A version of a version. **A cover is not here**: it realises the existing
/// work, as a performance does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum RecordingDerivationKind {
    Remix,
    Edit,
    Dub,
    Mashup,
    Sample,
}

#[allow(
    dead_code,
    reason = "the derivation model, proposed in domain-model.md; nothing mints one"
)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkDerivation {
    pub from: Ref<Work>,
    pub to: Ref<Work>,
    pub kind: WorkDerivationKind,
}

#[allow(
    dead_code,
    reason = "the edge is minted now — `identify derivations` writes it — but it lands in `Catalogue::derived` as a tuple; nothing yet reads one back out in this shape"
)]
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

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — catalogue traversal — where a recording appears, not which copies are held, which is what listings answers"
)]
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

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — reports a medium's numbering gaps, and no audit asks"
)]
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

    /// **A date as tags write it, and no further.**
    ///
    /// A sleeve saying 1995 is not claiming the first of January, and turning
    /// it into one would sort it ahead of every dated day in that year while
    /// looking like something somebody asserted.
    #[test]
    fn a_date_is_read_as_far_as_it_is_given() {
        assert_eq!(PartialDate::parse("1995"), Some(PartialDate::year(1995)));
        assert_eq!(
            PartialDate::parse("1996-03-16"),
            Some(PartialDate {
                year: 1996,
                month: Some(3),
                day: Some(16),
            })
        );
        assert_eq!(
            PartialDate::parse("1996-03"),
            Some(PartialDate {
                year: 1996,
                month: Some(3),
                day: None
            })
        );
        // A month that cannot be one is dropped, and the year beside it kept:
        // the year is worth more than the malformed part costs.
        assert_eq!(
            PartialDate::parse("1996-13-40"),
            Some(PartialDate::year(1996))
        );
        assert_eq!(PartialDate::parse(""), None);
        assert_eq!(PartialDate::parse("sometime"), None);
    }

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
                    kind: ReleaseType::Album,
                    // An album that is *also* a compilation, which is what
                    // MusicBrainz says and what the tags in this library
                    // carry: `album/compilation` (#133).
                    secondary: if n == 1 {
                        [ReleaseSecondary::Compilation].into_iter().collect()
                    } else {
                        Default::default()
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
                    label: None,
                    catalogue_number: None,
                    media: Some(1),
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
                    tracks: None,
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
                label: None,
                catalogue_number: Some("NON-2015".into()),
                media: Some(1),
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

#[cfg(test)]
mod dates {
    use super::*;

    /// A date as tags write it, which is three shapes and not one.
    #[test]
    fn a_partial_date_keeps_what_it_was_given() {
        assert_eq!(PartialDate::parse("1995"), Some(PartialDate::year(1995)));
        assert_eq!(
            PartialDate::parse("1996-03"),
            Some(PartialDate {
                year: 1996,
                month: Some(3),
                day: None
            })
        );
        assert_eq!(
            PartialDate::parse("1996-03-16"),
            Some(PartialDate {
                year: 1996,
                month: Some(3),
                day: Some(16)
            })
        );
    }

    /// **A messy date still gives up its year** (#86).
    ///
    /// The store had its own reader and it was the lenient one; sleeves carry
    /// these. Deleting it without carrying the leniency across would have made
    /// those files silently lose their year, which is the kind of loss nobody
    /// reports because there is nothing to see.
    #[test]
    fn a_year_is_found_inside_a_messy_date() {
        assert_eq!(PartialDate::parse("(1997)").map(|d| d.year), Some(1997));
        assert_eq!(
            PartialDate::parse("recorded 2016 live").map(|d| d.year),
            Some(2016)
        );
        assert_eq!(PartialDate::parse("1997/03/21").map(|d| d.year), Some(1997));
    }

    /// And what is not a year is absent rather than guessed.
    #[test]
    fn two_digits_is_not_a_year_anyone_can_place() {
        assert_eq!(PartialDate::parse("97"), None);
        assert_eq!(PartialDate::parse(""), None);
        assert_eq!(PartialDate::parse("unknown"), None);
    }
}
