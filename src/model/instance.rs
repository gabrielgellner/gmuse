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
    Artist, Concept, Device, Encoding, Facet, Hierarchy, Label, Medium, Recording, Release,
    ReleaseGroup, Role, Track, Work,
};
use crate::model::facet::{self, Described};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::log::{Catalogue, Fact, Record, compact, fold};
use crate::model::ord::OrdKey;
use crate::model::reference::{Cataloguable, CreditOn, RoleClass, Source};
use crate::model::spine::PartialDate;

/// What a scan says about one file.
///
/// The boundary between reading files and cataloguing them. #70 fills this
/// from real tags; the round trip fills it directly, because tag parsing is not
/// one of the risky decisions this slice exists to test.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Scanned {
    pub audio: AudioId,
    pub title: String,
    pub artist: String,
    /// The artist's MusicBrainz id, where the tags carry one — 92.7% of this
    /// library. Without it the artist is minted from the credit *string*, and
    /// a guest credit makes a new artist: `2Pac feat. Nate Dogg` is not `2Pac`
    /// to a hash (#89 §5).
    pub artist_mbid: Option<uuid::Uuid>,
    /// Whether the **album artist** tag names several — #145.
    ///
    /// The same trap as [`Self::credit_is_joined`] and it was unguarded: on
    /// *One from the Heart* the album artist id is `c3aeb863…/cefd9d3d…`, the
    /// primary half of which is Tom Waits, and the string is
    /// `Tom Waits and Crystal Gayle`. Writing one as the other renamed Tom
    /// Waits across 400 files and 26 albums.
    ///
    /// **A separate field, because the track's answer does not carry.** On
    /// that record the *track* artist is Crystal Gayle alone, with no slash,
    /// so `credit_is_joined` is false and guards nothing.
    pub album_credit_is_joined: bool,
    /// **Whether the file's name says a mastering tool re-encoded it** — #143.
    ///
    /// The one fact on this struct that comes from the path rather than the
    /// tags, and it is here because the path is about to stop existing: a
    /// placed file is named from the library, so import is the last moment the
    /// `_pn` suffix can be read.
    pub processed: bool,
    /// Whether the artist tag names **several** artists.
    ///
    /// Picard `/`-joins every credited artist's id, so the field says outright
    /// whether `Early James & Sierra Ferrell` is one artist's name or a credit
    /// listing two. It matters because the id is minted from the *primary*
    /// artist while the string names the whole credit — so naming an artist
    /// from a joined string renames the primary artist to the joined credit,
    /// globally, for every file that mentions them.
    pub credit_is_joined: bool,
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
    /// **What kind of release it is**, where the tags say — #133.
    ///
    /// Picard writes MusicBrainz's release-group type into
    /// `MusicBrainz Album Type` as a `/`-joined list: `album`,
    /// `album/compilation`, `album/compilation/dj-mix`. The first is the
    /// primary type and the rest are secondary, which is exactly the shape the
    /// spine holds.
    pub release_kind: crate::model::spine::ReleaseType,
    pub release_secondary: std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
    /// Where on the album, as the tags claim. Claims, because they are
    /// checkable and this library has media whose numbering does not add up.
    pub disc: Option<u16>,
    pub track: Option<u16>,
    /// **How many tracks the disc has, and how many discs the pressing has**
    /// — #134, the denominator a partial album needs.
    ///
    /// Claims, like the positions above: they come from the tags and they can
    /// be wrong. `None` is nobody saying, which is a third answer and not a
    /// zero — 12% of this library carries neither.
    pub track_total: Option<u16>,
    pub disc_total: Option<u16>,
    /// What key the tags say this is in — #136. Typed here, so a notation
    /// never travels past the boundary that read it.
    pub key: Option<crate::model::key::Key>,
    /// When **this pressing** came out, and when the **album** first did.
    ///
    /// Two dates because the tags carry two and they disagree for 820 files
    /// here: `Tri Repetae++` is a 1996 pressing of a 1995 album. `DATE` is
    /// the pressing, `ORIGINALDATE` the album — which is exactly the
    /// distinction #59's reissue trap is about, and it is in the tags rather
    /// than needing to be inferred.
    pub released: Option<PartialDate>,
    pub first_released: Option<PartialDate>,
    /// The imprint, from `TPUB` / `ORGANIZATION` / `LABEL`. 5,786 of 6,500
    /// files here carry one.
    pub label: Option<String>,
    /// The genres the tags name, one per term.
    ///
    /// **The weakest rung of the genre ladder** (#86). Genres in this model
    /// are concepts in a hierarchical codelist with a basis and a confidence,
    /// and nothing had ever asserted one — so a librarian that drew its own
    /// rows showed no genre at all for 6,437 of 6,485 of them. A tag is a
    /// claim like any other; it enters as `Source::TagVariant`, which is what
    /// a curated pass outranks.
    pub genres: Vec<String>,
    /// ReplayGain as the tags claim it. Every file here carries some.
    pub levels: crate::model::encoding::Levels,
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
    /// How the audio is shaped. Read while the file is open, like the bitrate,
    /// and the last two things a display row asks for that the model could not
    /// answer (#86).
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
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
    /// **Hold the bytes without naming the performance** — #160.
    ///
    /// The other half of the gate. Refusing a file outright leaves the library
    /// with no record that it exists, so every import re-refuses it and there
    /// is nowhere to say which ones are being worked on. This records what is
    /// true — that these bytes are here, and what their tags claim — and stops
    /// short of the one thing nothing has established: which performance this
    /// is.
    ///
    /// **No `EncodingBound`, and that is the whole point.** An encoding bound
    /// to a recording minted from its own bytes is exactly the answer-by-
    /// default §7 forbids. Unbound, it draws nowhere and browses nowhere, and
    /// `shelf query --unnamed` is the only thing that can see it — which is
    /// the work list a pass needs.
    ///
    /// Everything emitted here is a fact about the *bytes*: how they are
    /// encoded, how loud they are, whether a path says they were processed.
    /// None of it presumes a performance.
    pub fn hold_unnamed(&mut self, now: i64, scan: &[Scanned]) {
        for s in scan {
            let encoding = Id::<Encoding>::of_audio(&s.audio);
            self.append(
                now,
                Fact::EncodingDescribed {
                    encoding,
                    audio: s.audio,
                    format: s.format,
                    bitrate: s.bitrate,
                    length_ms: s.length_ms,
                    sample_rate: s.sample_rate,
                    channels: s.channels,
                },
            );
            self.append(
                now,
                Fact::EncodingClaimed {
                    encoding,
                    artist: s.artist.clone(),
                    title: s.title.clone(),
                },
            );
            if !s.levels.is_empty() {
                self.append(
                    now,
                    Fact::EncodingLevelled {
                        encoding,
                        levels: s.levels,
                    },
                );
            }
            if s.processed {
                self.append(
                    now,
                    Fact::EncodingProcessed {
                        encoding,
                        basis: Source::Path,
                    },
                );
            }
        }
    }

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
            let bound = known.bound.get(&encoding).copied();
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

            // **A joined credit names nobody.** The id belongs to the primary
            // artist and the string belongs to the credit, so writing one as
            // the other renamed Early James to `Early James & Sierra Ferrell`
            // across the library. The string is kept — as `as_credited` on the
            // credit, which is what a sleeve printed and where it belongs.
            if !s.credit_is_joined {
                self.append(
                    now,
                    Fact::ArtistNamed {
                        artist,
                        name: s.artist.clone(),
                    },
                );
            }
            self.append(
                now,
                Fact::RecordingCatalogued {
                    recording,
                    title: s.title.clone(),
                    artist,
                },
            );
            // **A version qualifier the title claims.** Matched against the
            // vocabulary rather than against "anything in brackets", so a
            // remixer's name does not become a concept — see `qualifiers_in`.
            //
            // `Matched` rather than `Provisional`: the parse did match a
            // term, and `Provisional` means nothing did. `TagVariant` because
            // a title is the weakest thing a claim can be read off, and a
            // human disagreeing lands beside this rather than on it.
            for label in crate::model::qualifier::qualifiers_in(&s.title) {
                self.assign_version(
                    now,
                    recording,
                    label,
                    Source::TagVariant,
                    Confidence::Matched,
                );
            }

            // **What the title says about who.** `(Nolah Remix)` names a
            // person, and throwing it away lost a credit the file was telling
            // us. Read conservatively — see `credits_in` for the measurement
            // that decided which role words are safe — and asserted as the
            // guess it is, so MusicBrainz confirming the same remixer lands
            // beside it rather than on it.
            for (role, name) in crate::model::qualifier::credits_in(&s.title) {
                let who = Id::<Artist>::of_artist(None, &name);
                self.append(
                    now,
                    Fact::ArtistNamed {
                        artist: who,
                        name: name.clone(),
                    },
                );
                self.credit(
                    now,
                    CreditOn::Recording(recording),
                    who,
                    role,
                    Source::TagVariant,
                    1,
                );
            }

            // **The genres the tags name**, one assignment per term (#86).
            //
            // A tag is a claim like any other and it enters at the weakest
            // rung, `TagVariant`, which is what a curated pass outranks. On
            // the recording rather than the album: a compilation's `Pop`
            // would otherwise be asserted over every track it holds (#74).
            for name in &s.genres {
                self.assert_concept(
                    now,
                    Described::Recording(recording),
                    facet::GENRE,
                    name,
                    Source::TagVariant,
                    Confidence::Provisional,
                );
            }

            // The key, at the same rung and for the same reason: a detector
            // wrote it, another detector would disagree, and a curated pass
            // outranks both (#136).
            if let Some(key) = s.key {
                self.assign_key(
                    now,
                    recording,
                    key,
                    Source::TagVariant,
                    Confidence::Provisional,
                );
            }

            // **The credit the projection used to invent.** It minted one role
            // from `b"performer"` and handed it to every artist in the
            // library, so a row that said "performer" said nothing — it was
            // the only thing it could say. An unqualified tag really is a
            // performer credit; the difference is that this one was asserted,
            // by a scan, at a stamp, and can be corrected.
            self.credit_as(
                now,
                CreditOn::Recording(recording),
                artist,
                "performer",
                Source::TagVariant,
                0,
                s.credit_is_joined.then(|| s.artist.clone()),
            );
            // An album, where the tags name one. Its id derives from the
            // release group MBID when there is one, so two devices agree on
            // the *album* as well as the track; otherwise from the album
            // artist and title together, because two artists' "Greatest Hits"
            // are not one album.
            //
            // **Both halves normalised** (#147), as every other named thing
            // is: `Id::of_name` goes through `normalise` and this went through
            // the raw string, so `Strange Time To Be Alive` and
            // `Strange Time to Be Alive` minted two albums. That breaks the
            // promise the whole model rests on — two devices agreeing without
            // coordination — because the capitalisation a tagger chose is not
            // something they can be expected to share.
            if let Some(album) = &s.album {
                let credited = s.album_artist.as_deref().unwrap_or(&s.artist);
                let group = match s.mb_release_group {
                    Some(mbid) => Id::<ReleaseGroup>::of_musicbrainz(mbid),
                    None => Id::<ReleaseGroup>::derived(
                        format!(
                            "{}\u{1f}{}",
                            crate::model::ingest::normalise(credited),
                            crate::model::ingest::normalise(album)
                        )
                        .as_bytes(),
                    ),
                };
                // The album's credit, by the same ladder. Its MusicBrainz id
                // is the *album artist's*, which is why it is not
                // `artist_mbid` — on a compilation those are different people.
                let group_artist = Id::<Artist>::of_artist(s.album_artist_mbid, credited);
                // **A joined album credit names nobody**, exactly as a joined
                // track credit does not (#145). The id is the primary
                // artist's and the string is the whole credit, so writing one
                // as the other is a global rename. The string is kept on the
                // credit below, which is where a sleeve's wording belongs.
                if !s.album_credit_is_joined {
                    self.append(
                        now,
                        Fact::ArtistNamed {
                            artist: group_artist,
                            name: credited.to_string(),
                        },
                    );
                }
                self.append(
                    now,
                    Fact::ReleaseGroupCatalogued {
                        group,
                        title: album.clone(),
                        artist: group_artist,
                        first_released: s.first_released,
                        kind: s.release_kind,
                        secondary: s.release_secondary.clone(),
                    },
                );
                // The album-level credit, which is what `file_under` asks for
                // first and what keeps a guest credit from splitting an album
                // off from its artist.
                self.credit_as(
                    now,
                    CreditOn::ReleaseGroup(group),
                    group_artist,
                    "performer",
                    Source::TagVariant,
                    0,
                    s.album_credit_is_joined.then(|| credited.to_string()),
                );
                // **The three levels a file actually sits in.** A scan sees
                // one pressing — the one on disk — so the release falls back
                // to the group rather than inventing a second; the disc
                // number, which used to ride on the placement, is now the
                // medium it was standing in for.
                let release = Id::<Release>::of_release(s.mb_release, group);
                // The imprint, where the sleeve names one. Catalogued first,
                // because a reference to a concept nothing names renders as
                // nothing.
                let label = s.label.as_deref().map(|name| {
                    let id = Id::<Label>::of_name(name);
                    self.append(
                        now,
                        Fact::LabelNamed {
                            label: id,
                            name: name.trim().to_string(),
                        },
                    );
                    id
                });
                self.append(
                    now,
                    Fact::ReleaseCatalogued {
                        release,
                        group,
                        title: album.clone(),
                        released: s.released,
                        label,
                        media: s.disc_total,
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
                        tracks: s.track_total,
                    },
                );
                let slot = Id::<Track>::of_track(medium, recording);
                // **Which copy sits in that slot.** The scan is the only thing
                // that ever knows: it is holding the file whose tags name the
                // album, and by the time anything else asks, a recording has
                // several placements and no way to say which one this copy is.
                //
                self.append(
                    now,
                    Fact::EncodingPlaced {
                        encoding,
                        track: slot,
                        // **What a scan can say**, which is the weakest rung
                        // there is: the tags said so. A seating decided by
                        // something that read the whole tracklist outranks it
                        // (#170), and this is what lets the fold tell them
                        // apart.
                        basis: Source::TagVariant,
                    },
                );
                self.append(
                    now,
                    Fact::TrackPlaced {
                        track: slot,
                        medium,
                        recording,
                        position: s.track.unwrap_or(0),
                        title_on_release: None,
                    },
                );
            }

            if !s.levels.is_empty() {
                self.append(
                    now,
                    Fact::EncodingLevelled {
                        encoding,
                        levels: s.levels,
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
                    sample_rate: s.sample_rate,
                    channels: s.channels,
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
            // **On the weakest rung**, because it is a claim the path makes
            // and a path never outranks a tag (#143). Recorded rather than
            // acted on: it is a complete record of provenance and a poor
            // predictor of damage — 9.7% of marked files fail the cutoff test
            // against 6.2% of the rest — and those are different jobs.
            if s.processed {
                self.append(
                    now,
                    Fact::EncodingProcessed {
                        encoding,
                        basis: Source::Path,
                    },
                );
            }
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

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — a relation is written by `Proposal::Related`, which goes through `facts_of` rather than through here"
    )]
    /// **Two artists are one act, and both rows stay** — #174.
    ///
    /// Not `merge`, which collapses them: AFX absorbed into Aphex Twin would
    /// take the moniker off the record it was released under, and *Analogue
    /// Bubblebath* belongs under AFX exactly as the sleeve says. This adds the
    /// ability to *also* ask for one shelf, by way of `file_under@1`, without
    /// giving up the four folders that are the right default.
    ///
    /// Not an alias either — [`Fact::AliasAsserted`] says one row goes by
    /// several names; this says two rows are one person.
    ///
    /// **`from` is the moniker and `to` the act.** Nothing derives that: a
    /// rule over the graph would elect a root that moves as the library grows,
    /// which is why MusicBrainz declares sixteen `is person` edges rather than
    /// computing one.
    pub fn relate_artists(
        &mut self,
        now: i64,
        from: Id<Artist>,
        to: Id<Artist>,
        kind: crate::model::reference::ArtistRelationKind,
    ) {
        self.append(
            now,
            Fact::ArtistRelated {
                from,
                to,
                kind,
                period: None,
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// **Drop records this pass added that the log already said** — #148.
    ///
    /// A re-import of an unchanged file should write nothing. It wrote
    /// everything: 54% of this library's log — 111,879 of 207,450 records —
    /// was one fact recorded again under a new stamp, because `import` had no
    /// equivalent of `apply`'s "already in the log" check.
    ///
    /// **Scoped to what one call added, and that is not fussiness.** A blanket
    /// rule would be wrong: `RecordingPlayed` carries no timestamp, so two
    /// plays of one track *are* the same fact told apart only by their stamps,
    /// and deduplicating those would count one play where there were two. An
    /// ingest produces no plays, which is what makes this safe here.
    pub fn drop_repeats_since(&mut self, before: &BTreeSet<Record>) {
        let said: std::collections::HashSet<String> = before
            .iter()
            .filter_map(|r| serde_json::to_string(&r.fact).ok())
            .collect();
        self.records.retain(|r| {
            before.contains(r)
                || !serde_json::to_string(&r.fact).is_ok_and(|encoded| said.contains(&encoded))
        });
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// Drop this instance's superseded holdings assertions.
    ///
    /// Safe at any moment, because folding a compacted log gives exactly what
    /// folding the original gives — and safe to run on one device and not
    /// another, because a compacted record that arrives back from a stale
    /// replica is simply superseded again.
    pub fn compact(&mut self) {
        self.records = compact(&self.records);
    }

    /// **Append one fact**, for a caller that has already decided what to say.
    ///
    /// The narrow door `import --into` uses: filing music into a collection is
    /// one fact per recording and needs none of the ladder the ingest methods
    /// carry.
    pub fn append_fact(&mut self, now: i64, fact: Fact) {
        self.append(now, fact);
    }

    /// **Retire a copy** — `storage-layout.md` §6, #159.
    ///
    /// A tombstone, so the library stops offering the copy while every row
    /// resting on it stays exactly where it is. That is what makes the second
    /// step — a purge, which does not exist — the only irreversible one.
    pub fn retire_encoding(&mut self, now: i64, encoding: Id<Encoding>) {
        self.append(now, Fact::EncodingRetired { encoding });
    }

    /// **A retired copy is back.** The other half of the pair, and not
    /// optional for the reason the fact's own doc gives: a step you cannot
    /// take back is one nobody takes.
    pub fn restore_encoding(&mut self, now: i64, encoding: Id<Encoding>) {
        self.append(now, Fact::EncodingRestored { encoding });
    }

    #[allow(
        dead_code,
        reason = "used by tests; #154 is served by `Proposal::Named`, and a proposal emits its facts rather than calling an `Instance` method — this stays the shortest way for a test to say the same thing"
    )]
    /// **Name an artist.** #154's `Proposal::Named` writes the same fact.
    pub fn rename_artist(&mut self, now: i64, artist: Id<Artist>, name: &str) {
        self.append(
            now,
            Fact::ArtistNamed {
                artist,
                name: name.to_string(),
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// **Claim that a concept describes a subject, on one axis** — #117.
    ///
    /// The one writer for every facet, where genre and version had one each.
    ///
    /// The concept is catalogued before the claim, because an id that no fact
    /// resolves is an id nobody can render. Idempotent: both ids derive from
    /// the label and its facet key, so a repeat is the same record.
    pub fn assert_concept(
        &mut self,
        now: i64,
        subject: Described,
        facet_key: &str,
        label: &str,
        basis: Source,
        confidence: Confidence,
    ) {
        let facet = Id::<Facet>::of_key(facet_key);
        let concept = Id::<Concept>::in_facet(facet_key, label);
        self.append(
            now,
            Fact::ConceptCatalogued {
                facet,
                concept,
                label: label.trim().to_string(),
                node_label: false,
            },
        );
        self.append(
            now,
            Fact::ConceptAsserted {
                subject,
                concept,
                basis,
                confidence,
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, and every production path now goes through `assert_concept` with the axis named — which is the point of #117, so this is the convenience that survives for the tests that read better with it"
    )]
    /// A genre, which is a concept on the `genre` axis.
    pub fn assign_genre(
        &mut self,
        now: i64,
        subject: Described,
        name: &str,
        basis: Source,
        confidence: Confidence,
    ) {
        self.assert_concept(now, subject, facet::GENRE, name, basis, confidence);
    }

    /// Say what key a recording is in — #136.
    ///
    /// **Typed in, written out canonically.** Every notation a tag uses parses
    /// to one [`Key`] at the boundary, so what reaches the log is the
    /// conventional spelling and `Cmin`, `8A` and `Am` cannot become three
    /// concepts for one key.
    pub fn assign_key(
        &mut self,
        now: i64,
        recording: Id<Recording>,
        key: crate::model::key::Key,
        basis: Source,
        confidence: Confidence,
    ) {
        self.assert_concept(
            now,
            Described::Recording(recording),
            facet::KEY,
            key.written(),
            basis,
            confidence,
        );
    }

    /// Say which version of a song a recording is.
    ///
    /// Refuses a node label: `mix-variant` is a place in the array, not a
    /// claim about a song, and ISO 25964's rule is that a node label does not
    /// take part in retrieval. Accepting it would fold to an assertion nobody
    /// can see — indistinguishable from one nobody made.
    pub fn assign_version(
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
        self.assert_concept(
            now,
            Described::Recording(recording),
            facet::VERSION,
            label,
            basis,
            confidence,
        );
        true
    }

    /// Credit an artist on something, in a role.
    ///
    /// The role is defined alongside, idempotently — the concept before the
    /// claim, as `assign_genre` does. Both ids derive from their names, so a
    /// repeat is the same record.
    pub fn credit(
        &mut self,
        now: i64,
        subject: CreditOn,
        artist: Id<Artist>,
        role: &str,
        basis: Source,
        position: u8,
    ) {
        self.credit_as(now, subject, artist, role, basis, position, None);
    }

    /// A credit, with what the sleeve printed where it differs from the
    /// artist's own name — a joined credit being the usual case.
    #[allow(clippy::too_many_arguments)]
    pub fn credit_as(
        &mut self,
        now: i64,
        subject: CreditOn,
        artist: Id<Artist>,
        role: &str,
        basis: Source,
        position: u8,
        as_credited: Option<String>,
    ) {
        let id = Id::<Role>::of_name(role);
        let class = crate::model::reference::ROLES
            .iter()
            .find(|(n, _)| *n == role)
            .map_or(RoleClass::Performer, |(_, c)| *c);
        self.append(
            now,
            Fact::RoleDefined {
                role: id,
                name: role.trim().to_string(),
                class,
            },
        );
        self.append(
            now,
            Fact::CreditAsserted {
                subject,
                artist,
                role: id,
                basis,
                position,
                as_credited,
                join_phrase: None,
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// Say a recording performs a work, cataloguing the work alongside.
    pub fn performs(&mut self, now: i64, recording: Id<Recording>, work: Id<Work>, title: &str) {
        self.append(
            now,
            Fact::WorkCatalogued {
                work,
                title: title.trim().to_string(),
            },
        );
        self.append(now, Fact::WorkPerformed { recording, work });
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
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
    pub fn rename_concept(
        &mut self,
        now: i64,
        facet_key: &str,
        concept: Id<Concept>,
        was: &str,
        now_called: &str,
    ) {
        self.append(
            now,
            Fact::ConceptCatalogued {
                facet: Id::<Facet>::of_key(facet_key),
                concept,
                label: now_called.trim().to_string(),
                node_label: false,
            },
        );
        self.append(
            now,
            Fact::HeadingRetired {
                subject: Cataloguable::Concept(concept),
                value: was.trim().to_string(),
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// Decide a concept does not apply. Shadows rather than deletes, and
    /// silences every asserter at once — a person saying "this is not live" is
    /// answering the question, not arguing with one source.
    pub fn retract_concept(
        &mut self,
        now: i64,
        subject: Described,
        facet_key: &str,
        label: &str,
        basis: Source,
    ) {
        self.append(
            now,
            Fact::ConceptRetracted {
                subject,
                concept: Id::<Concept>::in_facet(facet_key, label),
                basis,
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// Decide a version does not apply. Shadows rather than deletes.
    pub fn retract_version(
        &mut self,
        now: i64,
        recording: Id<Recording>,
        label: &str,
        basis: Source,
    ) {
        self.retract_concept(
            now,
            Described::Recording(recording),
            facet::VERSION,
            label,
            basis,
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// Write the version vocabulary into the log, so its ids resolve.
    ///
    /// Idempotent: every id derives from its label, so a second call appends
    /// records the set already holds.
    pub fn seed_versions(&mut self, now: i64) {
        let facet = Id::<Facet>::of_key(facet::VERSION);
        let hierarchy = Id::<Facet>::array(facet::VERSION);
        self.append(
            now,
            Fact::FacetDefined {
                facet,
                key: facet::VERSION.to_string(),
                label: "Version".to_string(),
            },
        );
        self.append(
            now,
            Fact::HierarchyDefined {
                hierarchy,
                facet,
                name: "Version".to_string(),
            },
        );
        for (label, parent, assignable) in crate::model::qualifier::SEED {
            let concept = Id::<Concept>::in_facet(facet::VERSION, label);
            self.append(
                now,
                Fact::ConceptCatalogued {
                    facet,
                    concept,
                    label: (*label).to_string(),
                    // **A node label groups and does not describe** — ISO
                    // 25964's rule, and the seed already knew which are which.
                    node_label: !assignable,
                },
            );
            if let Some(p) = parent {
                self.append(
                    now,
                    Fact::HierarchyNarrowed {
                        hierarchy,
                        parent: Id::<Concept>::in_facet(facet::VERSION, p),
                        child: concept,
                    },
                );
            }
        }
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    /// Withdraw a narrower-of edge. Shadows rather than deletes, so a stale
    /// replica re-delivering the edge cannot resurrect it.
    pub fn retract_narrowing(
        &mut self,
        now: i64,
        hierarchy: Id<Hierarchy>,
        facet_key: &str,
        parent: &str,
        child: &str,
    ) {
        self.append(
            now,
            Fact::HierarchyNarrowRetracted {
                hierarchy,
                parent: Id::<Concept>::in_facet(facet_key, parent),
                child: Id::<Concept>::in_facet(facet_key, child),
            },
        );
    }

    /// Name a collection, or rename one.
    ///
    /// Idempotent: the id derives from nothing, so the caller holds it and
    /// saying the same name twice is the same record.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; a playlist is still a file on disk until #92's UI half"
    )]
    /// Name a query, or rewrite one that already has this name.
    pub fn save_view(
        &mut self,
        now: i64,
        view: Id<crate::model::entity::View>,
        name: &str,
        source: &str,
    ) {
        self.append(
            now,
            Fact::ViewSaved {
                view,
                name: name.trim().to_string(),
                source: source.trim().to_string(),
            },
        );
    }

    pub fn name_collection(
        &mut self,
        now: i64,
        collection: Id<crate::model::entity::Collection>,
        name: &str,
        ordered: bool,
    ) {
        self.append(
            now,
            Fact::CollectionNamed {
                collection,
                name: name.to_string(),
                ordered,
            },
        );
    }

    /// Put a recording into a collection at a position.
    ///
    /// **The position is the caller's**, because only the caller knows where
    /// in the list this is going — [`OrdKey::between`] takes the neighbours it
    /// is landing between, and the fold has no notion of "the end".
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; a playlist is still a file on disk until #92's UI half"
    )]
    /// Record what a collection was frozen from — #95.
    pub fn froze(
        &mut self,
        now: i64,
        collection: Id<crate::model::entity::Collection>,
        source: &str,
    ) {
        self.append(
            now,
            Fact::CollectionFrozen {
                collection,
                source: source.trim().to_string(),
            },
        );
    }

    pub fn add_to_collection(
        &mut self,
        now: i64,
        collection: Id<crate::model::entity::Collection>,
        recording: Id<Recording>,
        ord: OrdKey,
    ) {
        self.append(
            now,
            Fact::CollectionEntryAdded {
                collection,
                recording,
                ord,
            },
        );
    }

    /// Take one entry out.
    ///
    /// The position identifies *which* entry, because a playlist may hold one
    /// recording twice and removing "the track" would be ambiguous.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; a playlist is still a file on disk until #92's UI half"
    )]
    pub fn remove_from_collection(
        &mut self,
        now: i64,
        collection: Id<crate::model::entity::Collection>,
        recording: Id<Recording>,
        ord: OrdKey,
    ) {
        self.append(
            now,
            Fact::CollectionEntryRemoved {
                collection,
                recording,
                ord,
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — nothing makes derived copies until the storage layer does (#132)"
    )]
    /// Say one file's bytes were made out of another's — #132.
    ///
    /// A transcode, and the claim that matters about it: `to` can be made
    /// again from `from`, so it need not be kept safe. `from` must be an
    /// encoding this library holds, or the claim is not true of anybody here.
    pub fn derives_encoding(&mut self, now: i64, from: Id<Encoding>, to: Id<Encoding>) {
        self.append(now, Fact::EncodingDerived { from, to });
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — nothing makes derived copies until the storage layer does (#132)"
    )]
    pub fn retract_encoding_derivation(&mut self, now: i64, from: Id<Encoding>, to: Id<Encoding>) {
        self.append(now, Fact::EncodingDerivationRetracted { from, to });
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — nothing imports derivation relations, and `derived[…]` reads the fold rather than writing it"
    )]
    /// Say one recording was made out of another — a remix, an edit, a dub
    /// (#79).
    ///
    /// **`from` is the original.** Reading the fact as "`to` is derived from
    /// `from`" is the only order that makes `derived[depth<=2]` mean what
    /// `domain-model.md` §5 says: two hops back toward the thing it came out
    /// of, not two hops forward into what came out of it.
    pub fn derives(
        &mut self,
        now: i64,
        from: Id<Recording>,
        to: Id<Recording>,
        kind: crate::model::spine::RecordingDerivationKind,
    ) {
        self.append(now, Fact::RecordingDerived { from, to, kind });
    }

    /// Take a derivation back. Shadows rather than deletes, for the reason
    /// every retraction here does: a stale replica would resurrect it.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — nothing imports derivation relations, and `derived[…]` reads the fold rather than writing it"
    )]
    pub fn retract_derivation(
        &mut self,
        now: i64,
        from: Id<Recording>,
        to: Id<Recording>,
        kind: crate::model::spine::RecordingDerivationKind,
    ) {
        self.append(now, Fact::RecordingDerivationRetracted { from, to, kind });
    }

    /// The other traversal, one level up: one work made out of another.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — nothing imports derivation relations, and `derived[…]` reads the fold rather than writing it"
    )]
    pub fn work_derives(
        &mut self,
        now: i64,
        from: Id<crate::model::entity::Work>,
        to: Id<crate::model::entity::Work>,
        kind: crate::model::spine::WorkDerivationKind,
    ) {
        self.append(now, Fact::WorkDerived { from, to, kind });
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — nothing imports derivation relations, and `derived[…]` reads the fold rather than writing it"
    )]
    pub fn retract_work_derivation(
        &mut self,
        now: i64,
        from: Id<crate::model::entity::Work>,
        to: Id<crate::model::entity::Work>,
        kind: crate::model::spine::WorkDerivationKind,
    ) {
        self.append(now, Fact::WorkDerivationRetracted { from, to, kind });
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the genre hierarchy is curated and no import path builds one"
    )]
    /// Say that one concept is narrower than another, inside one hierarchy.
    ///
    /// **Both concepts are catalogued first**, the same rule
    /// [`Self::assert_concept`] follows: an id that no fact resolves is an id
    /// nobody can render, and an edge naming two of them would give
    /// `levels genre@1` an ancestor it could not put a name on (#79).
    pub fn narrows(
        &mut self,
        now: i64,
        hierarchy: Id<Hierarchy>,
        facet_key: &str,
        parent: &str,
        child: &str,
    ) {
        let facet = Id::<crate::model::entity::Facet>::of_key(facet_key);
        for label in [parent, child] {
            self.append(
                now,
                Fact::ConceptCatalogued {
                    facet,
                    concept: Id::<Concept>::in_facet(facet_key, label),
                    label: label.trim().to_string(),
                    node_label: false,
                },
            );
        }
        self.append(
            now,
            Fact::HierarchyNarrowed {
                hierarchy,
                parent: Id::<Concept>::in_facet(facet_key, parent),
                child: Id::<Concept>::in_facet(facet_key, child),
            },
        );
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
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

    #[expect(
        dead_code,
        reason = "RecordingAcquired is folded and the migration writes it; no command asserts one yet"
    )]
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

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
    )]
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
            credit_is_joined: false,
            processed: false,
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
            released: None,
            first_released: None,
            genres: Vec::new(),
            label: None,
            levels: Default::default(),
            release_kind: Default::default(),
            album_credit_is_joined: false,
            key: None,
            track_total: None,
            disc_total: None,
            release_secondary: Default::default(),
            sample_rate: Some(44_100),
            channels: Some(2),
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

    /// **A track you do not own is a row that is greyed** — #155, end to end.
    ///
    /// One file of a two-track record, then the tracklist that record came
    /// with. Everything below turns on the ids converging: the file was tagged
    /// with the album's MusicBrainz id, so `import` and the tracklist mint the
    /// *same* release and the *same* medium, and the slot the file fills is
    /// the one the tracklist names rather than a second slot beside it.
    #[test]
    fn a_tracklist_draws_the_track_no_file_holds() {
        use crate::model::entity::Medium;
        use crate::model::proposal::{self, Holdings, Slotted, Subject};
        use crate::model::rows::Listing;

        let mbid = uuid::Uuid::from_u128(0x5e1e);
        let pressing = 0x5e1e_u128;
        // A one-disc release, said out loud: without a disc count `held_of`
        // answers `Unknown`, because an unseen second disc cannot be ruled
        // out. #134's third answer, and it is not what is under test here.
        let file = || {
            let mut s = scan(1, 1, Some(pressing));
            s.disc_total = Some(1);
            s
        };
        // Track 1 of 2, and the identity its tags carry — the same uuid the
        // tracklist will name for that slot.
        let held = uuid::Uuid::from_u128(1 << 32 | 1);
        let missing = uuid::Uuid::from_u128(0xabcd);

        let device = Id::from_uuid(uuid::Uuid::from_u128(1));
        let mut i = Instance::new(device);
        i.ingest(100, &[file()]);
        i.assert_holdings(
            100,
            [(
                Id::<crate::model::entity::Encoding>::of_audio(&file().audio),
                "/m/01.mp3".to_string(),
            )]
            .into_iter()
            .collect(),
        );

        /// Just enough of a library for `parse` to check a subject against.
        struct Shelf(crate::model::log::Catalogue);
        impl Holdings for Shelf {
            fn claim(
                &self,
                encoding: Id<crate::model::entity::Encoding>,
            ) -> Option<(String, String)> {
                self.0.claims.get(&encoding).cloned()
            }
            fn holds(&self, subject: Subject) -> bool {
                match subject {
                    Subject::Encoding(id) => self.0.encodings.contains_key(&id),
                    Subject::Recording(id) => self.0.recordings.contains_key(&id),
                    Subject::Medium(id) => self.0.media.contains_key(&id),
                    Subject::ReleaseGroup(id) => self.0.groups.contains_key(&id),
                    Subject::Artist(id) => self.0.artist_names.contains_key(&id),
                    Subject::Vocabulary => true,
                }
            }
            fn bound(&self, _: Id<crate::model::entity::Encoding>) -> Option<Id<Recording>> {
                None
            }
            fn identified(&self, _: Id<Recording>) -> Option<crate::model::basis::Basis> {
                None
            }
            fn slot(&self, _: Id<crate::model::entity::Encoding>) -> Option<proposal::Slot> {
                None
            }

            fn sleeve(&self, _: Id<crate::model::entity::Track>) -> Option<String> {
                None
            }

            fn named(&self, _: Id<crate::model::entity::Artist>) -> Option<String> {
                None
            }
        }

        let listed = proposal::Proposal::Tracklisted {
            release: mbid,
            position: 1,
            format: Some("CD".into()),
            tracks: vec![
                Slotted {
                    position: 1,
                    recording: held,
                    title: "Track 1".into(),
                    artists: vec!["Bodzin".into()],
                    title_on_release: None,
                },
                Slotted {
                    position: 2,
                    recording: missing,
                    title: "The One We Lack".into(),
                    artists: vec!["Bodzin".into()],
                    title_on_release: None,
                },
            ],
            album: None,
            title: None,
        };
        let text = serde_json::to_string(&listed).expect("a proposal serialises");
        let (records, report) =
            crate::model::proposal::parse(&text, device, 200, &Shelf(i.catalogue()));
        assert_eq!(report.applied, 1, "{report}");
        i.merge(200, &records);

        let cat = i.catalogue();
        assert_eq!(
            cat.media.len(),
            1,
            "**the ids converged** — a second medium here is a phantom disc"
        );
        assert_eq!(cat.tracks.len(), 2, "two slots on the one disc");

        let l = crate::model::seam::Librarian::from_catalogue(&cat);
        let medium = Id::<Medium>::of_medium(Id::<Release>::of_musicbrainz(mbid), 1);
        let slot_of = |rec: uuid::Uuid| {
            crate::model::rows::CopyId::On(Id::<Track>::of_track(
                medium,
                Id::<Recording>::of_basis(&crate::model::basis::Basis::MusicBrainz(rec)),
            ))
        };

        let ours = l.listed(slot_of(held)).expect("the track we hold");
        assert!(ours.held, "we have a file for it");

        let theirs = l.listed(slot_of(missing)).expect("the track we do not");
        assert!(!theirs.held, "**the greyed row** — the whole point of #155");
        assert_eq!(theirs.title, "The One We Lack");
        assert_eq!(
            theirs.album, "Luna",
            "under the record it is missing from, not under `No Album`"
        );
        assert_eq!(theirs.track, Some(2), "and at its place on it");

        // And knowing the record is two tracks long must not read as owning
        // both of them.
        let release = Id::<Release>::of_musicbrainz(mbid);
        assert_eq!(l.counted_of(release), Some((1, 2)));
        assert_eq!(l.held_of(release), crate::model::seam::Held::Partial);
    }

    /// **A path claim is recorded and never outranks a tag** — #143.
    ///
    /// The evidence rule this whole pass rests on: a filename and a tag are
    /// both claims, and the stronger *renders* while the weaker stays
    /// askable. `Source::Path` is the weakest rung there is, below
    /// `TagVariant`, because a DJ-pool filename carries a chart position where
    /// a track number goes — a rule that let the path win would be right about
    /// `_pn` and wrong about `37 Long Chicken` on a two-track release.
    #[test]
    fn a_mastering_marker_is_recorded_on_the_weakest_rung() {
        use crate::model::reference::Source;

        let mut marked = scan(1, 1, None);
        marked.processed = true;
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[marked.clone(), scan(1, 2, None)]);
        let cat = i.catalogue();

        let enc = Id::<crate::model::entity::Encoding>::of_audio(&marked.audio);
        assert_eq!(
            cat.processed
                .get(&enc)
                .map(|by| by.iter().copied().collect::<Vec<_>>()),
            Some(vec![Source::Path]),
            "the marker did not land, or landed on the wrong rung"
        );
        assert_eq!(cat.processed.len(), 1, "an unmarked file was marked");

        assert!(
            Source::Path < Source::TagVariant,
            "**a path must never outrank a tag** — `line_of` takes the strongest \
             basis present, so this ordering is the whole of the rule"
        );
    }

    /// **A retired disc stops being drawn** — #159, and the case that found it.
    ///
    /// `Early James — Strange Time to Be Alive` drew 24 rows for a 12-track
    /// record: the album had been retired by moving its folder to `.departed`
    /// by hand, and nothing recorded that. A *holding* lapses when a scan
    /// stops seeing a file; a *claim* does not, so every recording, slot,
    /// medium and release group stayed catalogued.
    ///
    /// #155 draws a slot whose disc a copy came off, which that disc still
    /// had. The tombstone is what takes it away.
    #[test]
    fn a_disc_whose_copies_are_retired_stops_being_drawn() {
        use crate::model::rows::Listing;

        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 1, Some(0x5e1e)), scan(1, 2, Some(0x5e1e))]);
        let l = crate::model::seam::Librarian::from_catalogue(&i.catalogue());
        assert_eq!(l.rows().len(), 2, "two files, two rows");

        // Retire both copies, as `remove` does once the bytes have gone.
        let now = 200;
        for track in [1u16, 2] {
            let enc = Id::<crate::model::entity::Encoding>::of_audio(&scan(1, track, None).audio);
            i.retire_encoding(now, enc);
        }
        let l = crate::model::seam::Librarian::from_catalogue(&i.catalogue());
        assert_eq!(
            l.rows().len(),
            0,
            "a disc the library holds no copy of is not a record you are short of"
        );
    }

    /// **A retag leaves a disc behind, and it must not draw** — #155.
    ///
    /// Found on the real library, by looking at it. `The Bug — London Zoo`
    /// showed track 11 twice, once with a length of 0: the album had been
    /// retagged, which mints a new release group, pressing, disc and slots and
    /// moves the copies to them. The old disc stays — the log is append-only
    /// and nothing retracts a slot — and while a row was a *copy* it drew
    /// nothing. Drawing appearances made every one of them a second, empty row
    /// beside the real one. **384 of them on this library.**
    ///
    /// The rule that fixes it is the honest reading of what a greyed row
    /// means: an empty slot is a track missing from a record you own part of.
    /// A disc no file here came off is not a record you are short of — it is
    /// an identity that moved.
    #[test]
    fn a_disc_nothing_here_came_off_is_not_drawn() {
        use crate::model::rows::{CopyId, Listing};

        let was = 0x5e1e_u128;
        let now = 0x5e2e_u128;
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan(1, 1, Some(was))]);
        // The same bytes, retagged onto another pressing and rescanned. The
        // encoding is minted from the audio, so it is the same encoding and
        // its placement moves; the slot it left does not go anywhere.
        i.ingest(200, &[scan(1, 1, Some(now))]);

        let cat = i.catalogue();
        assert_eq!(
            cat.tracks.len(),
            2,
            "the abandoned slot is still in the log"
        );
        assert_eq!(cat.media.len(), 2, "and so is the disc it sits on");

        let l = crate::model::seam::Librarian::from_catalogue(&cat);
        let drawn: Vec<CopyId> = l.rows();
        assert_eq!(
            drawn.len(),
            1,
            "one file, one row — the abandoned disc drew a second, empty one"
        );

        let slot = drawn[0].slot().expect("an appearance");
        let medium = cat.tracks[&slot].medium;
        assert_eq!(
            cat.media[&medium].release,
            Id::<Release>::of_musicbrainz(uuid::Uuid::from_u128(now)),
            "and the row that survived is the one on the pressing we hold"
        );
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

#[cfg(test)]
mod credit_tests {
    //! **What an artist did, said by somebody rather than assumed.**
    //!
    //! The projection used to mint one role from `b"performer"` and push a
    //! credit naming it for every recording in the library. Those rows had no
    //! stamp, no author and no way to be corrected, and a role that was the
    //! only role said nothing at all.

    use super::*;
    use crate::model::reference::{CreditOn, RoleClass};

    fn scan(title: &str, artist: &str) -> Scanned {
        Scanned {
            audio: crate::audio_id::AudioId {
                scheme: 1,
                hash: title.len() as u128,
            },
            title: title.into(),
            artist: artist.into(),
            artist_mbid: None,
            credit_is_joined: false,
            processed: false,
            identity: Some(crate::model::basis::Basis::MusicBrainz(
                uuid::Uuid::from_u128(title.len() as u128),
            )),
            album: Some("Luna".into()),
            album_artist: Some("Bodzin".into()),
            album_artist_mbid: None,
            disc: Some(1),
            track: Some(1),
            mb_release_group: Some(uuid::Uuid::from_u128(0x9e11)),
            mb_release: None,
            length_ms: 200_000,
            format: Some(crate::model::encoding::Format::Mp3),
            bitrate: 320,
            released: None,
            first_released: None,
            genres: Vec::new(),
            label: None,
            levels: Default::default(),
            release_kind: Default::default(),
            album_credit_is_joined: false,
            key: None,
            track_total: None,
            disc_total: None,
            release_secondary: Default::default(),
            sample_rate: Some(44_100),
            channels: Some(2),
        }
    }

    /// **Capitalisation does not make two albums** — #147.
    ///
    /// Every other named thing mints through `normalise`; the release-group
    /// fallback went through the raw string, so `Strange Time To Be Alive` and
    /// `Strange Time to Be Alive` were two albums. That breaks the promise the
    /// model rests on — two devices agreeing without coordination — because
    /// the capitalisation a tagger chose is not something they share.
    #[test]
    fn one_album_spelled_two_ways_is_one_album() {
        // **No release-group id**, which is what puts the id on the fallback
        // this test is about. With one the album converges on MusicBrainz and
        // the spelling never enters into it.
        let of = |album: &str, artist: &str| Scanned {
            album: Some(album.into()),
            album_artist: Some(artist.into()),
            mb_release_group: None,
            ..scan(album, artist)
        };
        let mut i = Instance::new(crate::fixture::device());
        i.ingest(
            100,
            &[
                of("Strange Time To Be Alive", "Early James"),
                of("Strange Time to Be Alive", "Early James"),
                of("strange time to be alive", "EARLY JAMES"),
            ],
        );
        assert_eq!(
            i.catalogue().groups.len(),
            1,
            "one album became several because a tagger pressed shift"
        );

        // **And two albums are still two.** Normalising must not merge things
        // that differ in more than case — two artists' "Greatest Hits" are not
        // one record, which is why the credit is half of the id.
        let mut j = Instance::new(crate::fixture::device());
        j.ingest(
            100,
            &[
                of("Greatest Hits", "One Artist"),
                of("Greatest Hits", "Another Artist"),
                of("Greatest Hits Vol. 2", "One Artist"),
            ],
        );
        assert_eq!(j.catalogue().groups.len(), 3);
    }

    /// **A joined album credit does not rename the primary artist** — #145.
    ///
    /// Found by `shelf render`: 400 files across 26 albums proposed to file
    /// under `Tom Waits and Crystal Gayle`, which is his whole discography.
    /// The album artist id on *One from the Heart* is `c3aeb863…/cefd9d3d…`
    /// and its primary half is Tom Waits, so the joined string was written as
    /// his name and every release group crediting him read it back.
    ///
    /// `credit_is_joined` guarded the *track* credit and could not help: on
    /// that record the track artist is Crystal Gayle alone.
    #[test]
    fn a_joined_album_credit_does_not_rename_the_artist_it_starts_with() {
        let waits = uuid::Uuid::from_u128(0xc3aeb863);
        let solo = Scanned {
            artist: "Tom Waits".into(),
            artist_mbid: Some(waits),
            album: Some("Bone Machine".into()),
            album_artist: Some("Tom Waits".into()),
            album_artist_mbid: Some(waits),
            ..scan("Earth Died Screaming", "Tom Waits")
        };
        let duet = Scanned {
            artist: "Crystal Gayle".into(),
            artist_mbid: Some(uuid::Uuid::from_u128(0xcefd9d3d)),
            album: Some("One From the Heart".into()),
            album_artist: Some("Tom Waits and Crystal Gayle".into()),
            // Picard writes the *primary* id here, and the joined field is
            // what says there were two.
            album_artist_mbid: Some(waits),
            album_credit_is_joined: true,
            ..scan("Is There Any Way Out of This Dream", "Crystal Gayle")
        };

        let mut i = Instance::new(crate::fixture::device());
        i.ingest(100, &[solo, duet]);
        let cat = i.catalogue();
        let who = Id::<crate::model::entity::Artist>::of_artist(Some(waits), "Tom Waits");
        assert_eq!(
            cat.artist_names.get(&who).map(String::as_str),
            Some("Tom Waits"),
            "the joined credit renamed the artist it merely starts with"
        );

        // **The wording is not lost.** It is on the credit, which is where a
        // sleeve's phrasing belongs.
        let joined = cat
            .credits
            .values()
            .filter_map(|c| c.as_credited.as_deref())
            .any(|s| s == "Tom Waits and Crystal Gayle");
        assert!(joined, "the credit lost what the sleeve printed");
    }

    /// A scan credits the track artist and the album artist, and the role it
    /// names resolves to something a person can read.
    #[test]
    fn a_scan_asserts_the_credits_it_can_see() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan("Callisto", "Bodzin")]);
        let cat = i.catalogue();

        let performer = Id::<Role>::of_name("performer");
        assert_eq!(
            cat.roles.get(&performer),
            Some(&("performer".to_string(), RoleClass::Performer)),
            "a credit named a role no fact defines, so nothing can render it"
        );
        assert_eq!(cat.credits.len(), 2, "the track's credit and the album's");
        assert!(cat.credits.keys().all(|(_, _, role, _)| *role == performer));
        assert!(
            cat.credits
                .keys()
                .any(|(on, _, _, _)| matches!(on, CreditOn::ReleaseGroup(_))),
            "the album-level credit is what `file_under` asks for first"
        );
    }

    /// **A title that says what version this is, routed to a fact.**
    ///
    /// The claim was there in every filename all along and had nowhere to go,
    /// so the matcher's only move was to *downgrade* a match it was suspicious
    /// of. This records the suspicion instead.
    #[test]
    fn a_version_in_the_title_becomes_a_qualifier() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan("212 (Instrumental)", "Azealia Banks")]);

        let cat = i.catalogue();
        let (_, concept, basis) = cat.assertions.keys().next().expect("no version");
        assert_eq!(
            *concept,
            Id::<Concept>::in_facet(facet::VERSION, "instrumental")
        );
        assert_eq!(
            *basis,
            Source::TagVariant,
            "a title is the weakest thing a claim can be read off, and a \
             person disagreeing has to land beside this rather than on it"
        );
        assert_eq!(
            cat.concepts.get(concept).map(|c| c.label.as_str()),
            Some("instrumental"),
            "the concept was asserted before anything named it"
        );
    }

    /// A remixer's name is not a version — but it is not nothing either.
    /// One title, two different claims, each to its own fact.
    #[test]
    fn a_remix_in_the_title_is_a_credit_and_not_a_qualifier() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan("Phobos (Pan-Pot Remix)", "Pig&Dan")]);
        let cat = i.catalogue();

        assert!(cat.assertions.is_empty(), "a person is not a version");

        let remixer = Id::<Role>::of_name("remixer");
        let (_, artist, _, basis) = cat
            .credits
            .keys()
            .find(|(_, _, role, _)| *role == remixer)
            .expect("the title named who and nothing wrote it down");
        assert_eq!(*artist, Id::<Artist>::of_artist(None, "Pan-Pot"));
        assert_eq!(
            cat.artist_names.get(artist).map(String::as_str),
            Some("Pan-Pot"),
            "an artist the credit names but nothing else does"
        );
        assert_eq!(
            *basis,
            Source::TagVariant,
            "read out of a title, and a MusicBrainz credit has to land beside \
             this rather than on top of it"
        );
    }

    /// **A guess and a confirmation are two rows.** The reason the basis is in
    /// the credit's key: folded to one, the agreement between a title parse
    /// and an authority — the most useful thing the pair says — is lost.
    #[test]
    fn a_confirmed_remixer_does_not_overwrite_the_guessed_one() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan("Phobos (Pan-Pot Remix)", "Pig&Dan")]);
        let rec = *i.catalogue().recordings.keys().next().unwrap();

        i.credit(
            200,
            CreditOn::Recording(rec),
            Id::<Artist>::of_artist(None, "Pan-Pot"),
            "remixer",
            Source::MusicBrainz,
            1,
        );

        let remixer = Id::<Role>::of_name("remixer");
        let bases: Vec<Source> = i
            .catalogue()
            .credits
            .keys()
            .filter(|(_, _, role, _)| *role == remixer)
            .map(|(_, _, _, basis)| *basis)
            .collect();
        assert_eq!(bases, vec![Source::TagVariant, Source::MusicBrainz]);
    }

    /// A length variant is a version, and the title says which.
    #[test]
    fn a_radio_edit_in_the_title_becomes_a_qualifier() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[scan("Alors on danse (Radio Edit)", "Stromae")]);
        assert_eq!(
            i.catalogue()
                .assertions
                .keys()
                .map(|(_, c, _)| *c)
                .collect::<Vec<_>>(),
            vec![Id::<Concept>::in_facet(facet::VERSION, "radio-edit")]
        );
    }

    /// **A joined credit names nobody.**
    ///
    /// The id is minted from the *primary* artist and the string names the
    /// whole credit, so writing one as the other renames the primary artist to
    /// the joined credit — globally, for every file that mentions them, since
    /// the name is a last-writer register. On this library that renamed
    /// `Early James` to `Early James & Sierra Ferrell` from a single track.
    #[test]
    fn a_joined_credit_does_not_rename_the_primary_artist() {
        let mbid = uuid::Uuid::from_u128(0xea41);
        let mut alone = scan("Pigsty", "Early James");
        alone.artist_mbid = Some(mbid);
        let mut joined = scan("Real Low Down Lonesome", "Early James & Sierra Ferrell");
        joined.artist_mbid = Some(mbid);
        joined.credit_is_joined = true;

        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.ingest(100, &[alone, joined]);
        let cat = i.catalogue();

        let artist = Id::<Artist>::of_artist(Some(mbid), "anything");
        assert_eq!(
            cat.artist_names.get(&artist).map(String::as_str),
            Some("Early James"),
            "the joined credit renamed the artist it is only the primary of"
        );
        assert!(
            cat.credits
                .values()
                .any(|c| c.as_credited.as_deref() == Some("Early James & Sierra Ferrell")),
            "and the string the sleeve printed was thrown away"
        );
    }

    /// **One artist, two roles, two rows.** The role is part of the claim's
    /// identity, so producing and performing do not overwrite each other.
    #[test]
    fn an_artist_in_two_roles_is_two_credits() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        let rec = Id::<Recording>::derived(b"callisto");
        let artist = Id::<Artist>::of_artist(None, "Bodzin");
        i.credit(
            100,
            CreditOn::Recording(rec),
            artist,
            "performer",
            Source::TagVariant,
            0,
        );
        i.credit(
            200,
            CreditOn::Recording(rec),
            artist,
            "producer",
            Source::TagVariant,
            0,
        );
        assert_eq!(i.catalogue().credits.len(), 2);
    }

    /// **Re-crediting moves the position rather than adding a credit.** How a
    /// sleeve orders a credit is correctable, and correcting it must not mint
    /// a second one.
    #[test]
    fn correcting_a_position_does_not_mint_a_second_credit() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        let rec = Id::<Recording>::derived(b"callisto");
        let artist = Id::<Artist>::of_artist(None, "Bodzin");
        i.credit(
            100,
            CreditOn::Recording(rec),
            artist,
            "performer",
            Source::TagVariant,
            0,
        );
        i.credit(
            200,
            CreditOn::Recording(rec),
            artist,
            "performer",
            Source::TagVariant,
            2,
        );
        let cat = i.catalogue();
        assert_eq!(cat.credits.len(), 1);
        assert_eq!(cat.credits.values().next().unwrap().position, 2);
    }

    /// The role id comes from its name, so two devices define the codelist
    /// without meeting — and it is **not** the byte string the projection used
    /// to hash.
    #[test]
    fn a_role_id_is_minted_from_its_name() {
        assert_eq!(
            Id::<Role>::of_name("Performer"),
            Id::<Role>::of_name(" performer ")
        );
        assert_ne!(
            Id::<Role>::of_name("performer"),
            Id::<Role>::of_name("producer")
        );
        // **And it agrees with the id the projection used to mint inline**,
        // because `performer` normalises to itself. That is luck rather than
        // design, and it is worth writing down: it means the credits already
        // on disk need no migration, where any role with a capital in it
        // would have.
        assert_eq!(
            Id::<Role>::of_name("performer"),
            Id::<Role>::derived(b"performer")
        );
    }

    /// **A medley performs two works and the spine holds one**, so the folded
    /// answer is none — and both rows stay in the catalogue for anything that
    /// can read a set. An arbitrary winner would be the projection inventing
    /// again, in a smaller way.
    #[test]
    fn a_recording_that_performs_two_works_folds_to_neither() {
        use crate::model::seam::Librarian;

        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        let rec = Id::<Recording>::derived(b"medley");
        i.append(
            50,
            Fact::RecordingCatalogued {
                recording: rec,
                title: "Medley".into(),
                artist: Id::<Artist>::of_artist(None, "Casals"),
            },
        );
        let one = Id::<Work>::of_musicbrainz(uuid::Uuid::from_u128(1));
        i.performs(100, rec, one, "Prelude");
        assert_eq!(
            Librarian::from_catalogue(&i.catalogue()).spine.recordings[&rec]
                .work
                .map(|w| w.id()),
            Some(one),
            "one performance is an answer"
        );

        i.performs(
            200,
            rec,
            Id::<Work>::of_musicbrainz(uuid::Uuid::from_u128(2)),
            "Allemande",
        );
        let cat = i.catalogue();
        assert_eq!(cat.performances.len(), 2, "the second was dropped");
        assert_eq!(cat.works.len(), 2);
        assert!(
            Librarian::from_catalogue(&cat).spine.recordings[&rec]
                .work
                .is_none(),
            "the projection picked a winner where the data names two"
        );
    }
}
