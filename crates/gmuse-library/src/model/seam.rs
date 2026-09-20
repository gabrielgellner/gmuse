//! The seam — issue #77.
//!
//! **The library answers questions and accepts changes; the player asks and
//! plays.** This module is where that sentence becomes a type.
//!
//! Today `src/ui/` reaches into a `Scan` and handles `PathBuf`s directly, so
//! the player and the librarian are one program by accident rather than by
//! design. [`Librarian`] is the surface that ends that: everything the player
//! needs, expressed in the model's terms, and nothing that requires it to know
//! where a file sits or what a tag key is called.
//!
//! ## Two directions, and only one of them holds yet
//!
//! *The model must not know about the player* holds today and is asserted, so
//! it cannot quietly stop holding. Nothing under `src/model/` names `ui`,
//! `audio`, `cmd`, `ratatui` or `crossterm`; its only reaches outside are
//! `audio_id`, which is identity rather than playback, and `lofty`, which is
//! the tag boundary.
//!
//! *The player must not know about the store* does **not** hold yet, and
//! pretending otherwise would be worse than saying so. [`tests::the_leak`]
//! counts what is left and refuses to let it grow, which turns a 10,000-line
//! migration into something with a number on it that can only go down.

use crate::model::encoding::{Encoding, Encodings, Format, Verdict};
use crate::model::entity::{Collection, Device, Recording, ReleaseGroup, Track};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::facet::Described;
use crate::model::ord::OrdKey;
use crate::model::provenance::Provenance;
use crate::model::reference::{CreditOn, Reference};
use crate::model::rows::Drawn;
use crate::model::spine::Spine;
use crate::model::vocabulary::Vocabulary;
use crate::projection::catalogue::{Catalogue, Collected};

/// What the player is allowed to ask, and what it is allowed to ask for.
///
/// Deliberately small. The four things `src/ui/` asks a `Scan` for today are
/// *get*, *iterate*, *upsert* and *look up by path* — and the last of those is
/// the one that must not survive the move, because a path is a fact about one
/// device's copy and the player has no business holding one.
pub struct Librarian {
    pub spine: Spine,
    pub reference: Reference,
    /// The genre codelist and its hierarchy.
    ///
    /// **This carried an `#[expect(dead_code)]` until #116**, and the
    /// attribute retired itself the moment `depth` read the edges — which is
    /// what `expect` is for and why #113 chose it over `allow`.
    pub vocabulary: Vocabulary,
    pub encodings: Encodings,
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — held so the librarian owns the whole fold; nothing reads it back through the seam"
    )]
    pub provenance: Provenance,
    /// Folded from the log, never stored as numbers. #67's rule decides a
    /// rating; a play is counted.
    pub ratings: std::collections::BTreeMap<Id<Recording>, u8>,
    pub plays: std::collections::BTreeMap<Id<Recording>, u32>,
    /// Every genre assigned to each subject, by name.
    ///
    /// Folded down from `Catalogue::genres`, which keys by
    /// `(subject, concept, source)`: several sources may assert the same
    /// concept, and each carries its own confidence. Collapsed to the distinct
    /// labels here, most confident first, so a display with room for one shows
    /// the best-supported and a filter can ask about any of them.
    ///
    /// **Keyed by axis too** (#117), so `genre` and `version` share one map
    /// and a third axis needs no fourth field.
    pub concepts:
        std::collections::BTreeMap<(Described, Id<crate::model::entity::Facet>), Vec<String>>,
    /// Every row this library can draw, materialised once (#86).
    ///
    /// `Listed` borrows and the librarian computes — `file_under` from the
    /// album-level credit, `album` from the slot — so there is nothing to lend
    /// a view of unless it is kept. Built at the end of the fold and lent from
    /// there.
    rows: std::collections::BTreeMap<crate::model::rows::CopyId, Drawn>,
    /// Where this device can open each row, from the holdings it asserted.
    ///
    /// Empty until [`Librarian::here`] says which device this is. The
    /// librarian knows every holding; which of them is reachable *now* is a
    /// fact about the machine it is running on.
    locators: std::collections::BTreeMap<Id<Encoding>, String>,
    /// And the inverse, for reading a playlist back.
    ///
    /// **Many paths, one row.** Two files with identical bytes are one
    /// encoding, so both locators name it — which is the right answer for an
    /// `.m3u` that happens to list the copy you deleted.
    held_at: std::collections::BTreeMap<String, Id<Encoding>>,
    /// What each collection is called.
    pub collections: BTreeMap<Id<Collection>, Collected>,
    /// **Every claim, with who made it and how sure** — #125.
    ///
    /// `concepts` above is the collapsed form: distinct labels, best first,
    /// which is what a row needs. It drops the basis and the confidence, and
    /// those are exactly what the info panel exists to show — #96 made a
    /// retraction shadow rather than delete precisely so that "somebody ruled
    /// this out" stays a different answer from "nobody said it", and a
    /// collapsed list cannot express either.
    pub claims: BTreeMap<(Described, Id<crate::model::entity::Facet>), Vec<Claimed>>,
    /// What each recording's identity rests on, where an authority named it.
    pub identified: BTreeMap<Id<Recording>, crate::model::basis::Basis>,
    /// Which recordings were folded into which — `(was, is now)`.
    pub merged: Vec<(Id<Recording>, Id<Recording>)>,
    /// **Which release group describes a recording that describes nothing itself** —
    /// #74, #253.
    ///
    /// A row takes the recording's own concepts and falls back to a group's. Which
    /// group is the whole question: a recording held on its album *and* on a
    /// compilation was taking the group of the slot being drawn, so browsing the
    /// compilation filed a rock song under `Pop`.
    ///
    /// **Non-compilations first, then lowest id.** The tie-break is not cosmetic — it
    /// makes the answer a rule rather than whichever release the iteration reached
    /// first, which is the same reason `row_of` fills in the canonical copy.
    ///
    /// Built once here rather than asked per row. `slots_of` is a full scan of every
    /// track, and calling it from `row_of_via` would be O(rows × tracks) — the
    /// per-row scan #123 spent a milestone removing.
    pub described_by: BTreeMap<Id<Recording>, Id<ReleaseGroup>>,
    /// Which files were made out of which — `(source, derived)`, #132.
    pub encoding_derived: Vec<(Id<Encoding>, Id<Encoding>)>,
    /// **How much of each pressing is here, worked out once** — #134, #137.
    ///
    /// It was a scan of the whole spine, per row, and the fold went from
    /// 27 ms to 124 ms the moment the library carried counts to scan. Same
    /// shape as #121 and #123: the answer is per release and the question was
    /// being asked per copy.
    pub held: std::collections::BTreeMap<Id<crate::model::entity::Release>, Counted>,
    /// The log, kept so the info panel can say when something became true and
    /// on which machine — #128.
    ///
    /// **What was recorded, not what a scan re-derives.** Ingesting 6,500
    /// files mints some 160,000 facts every launch, with this launch's stamps;
    /// showing those beside the handful somebody actually wrote would drown
    /// the ones worth reading in noise that says only "the disk was walked".
    ///
    /// **A count, where this was the whole record set** — #256. Every fold cloned the
    /// log into here: **75.5 MB of the 102 MB** a fold of the real library kept, held so
    /// that a caller would not have to ask. The only production reader took its `len()`,
    /// and the info panel it was carried for is still *proposal, not built*.
    ///
    /// `Desk::records()` is the door now, and it is a question rather than a copy.
    pub record_count: usize,
    /// Every collection entry, in list order — see [`Catalogue::entries`].
    collected: BTreeSet<(Id<Collection>, OrdKey, Id<Recording>)>,
    /// Which slot each copy sits in — issue #78.
    ///
    /// The field that makes a row's album a fact rather than a choice. Without
    /// it, `album_of` asked a *recording* which album it was on, and a
    /// recording is on every release that holds it.
    pub placed: std::collections::BTreeMap<Id<Encoding>, Id<Track>>,
    /// `placed` the other way round: which copies sit in each slot — #270.
    ///
    /// **The index that took a tree from 830 ms to a fraction of it.** `shown_copy`
    /// walked every entry of `placed` to find the copies on one track, and
    /// `Tree::grouped` asks it twice per row — 7,285 rows over 7,374 placements is
    /// fifty million comparisons for the shape of the tree, which was 526 ms of
    /// the tree's 830. #123's precedent, "walking them costs less than an index",
    /// was written for one row; a walk per row is the case it did not cover.
    ///
    /// Kept beside `placed` by [`Librarian::reindex_placed`]; a test that writes
    /// `placed` directly reindexes after, or `shown_copy` answers nothing for it.
    placed_on: std::collections::BTreeMap<Id<Track>, Vec<Id<Encoding>>>,
}

impl Default for Librarian {
    fn default() -> Self {
        Self::new()
    }
}

/// One recorded fact, as the info panel shows it — #128.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Happened {
    /// Wall clock, seconds.
    pub at: i64,
    pub device: String,
    /// The fact's own name, from its wire form.
    pub name: String,
    pub detail: String,
    /// Whether this took something back rather than saying it.
    pub withdrawal: bool,
}

/// Whether a fact's name says it withdraws something.
///
/// **By the naming convention, and that is a stated limit.** The withdrawals
/// spell themselves three ways — `Retracted`, `Removed`, `Dropped` — and a
/// fourth spelling would go unmarked here. The cost of that is cosmetic: an
/// unmarked withdrawal still shows, with its own name, which reads as a
/// withdrawal to a person. A hand-kept list of variants would go stale in the
/// same place and fail silently in a worse one.
fn is_withdrawal(name: &str) -> bool {
    ["Retracted", "Removed", "Dropped"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// One claim about a subject: what was said, by whom, and how sure — #125.
///
/// **Retracted ones are kept.** `assertions_retracted` records the strongest
/// basis that ruled a concept out (#96), and dropping the row here would make
/// "somebody decided against this" indistinguishable from "nobody ever said
/// it" — which is the whole reason the retraction is a row rather than a
/// delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    pub label: String,
    /// Who said it, and how sure.
    ///
    /// **`None` when only the retraction is left.** The fold *removes* the
    /// shadowed rows — `retract` sweeps every asserter of the pair — so a
    /// concept somebody ruled out has a retraction and no assertion behind it.
    /// Saying `manual` asserted it would be a lie; saying nothing at all would
    /// lose the fact that it was ruled out. The log still holds the assertion
    /// that was shadowed, and #128's facts section is where it shows.
    pub said: Option<(
        crate::model::reference::Source,
        crate::model::ingest::Confidence,
    )>,
    /// The basis that ruled it out, if one did.
    pub retracted: Option<crate::model::reference::Source>,
}

/// How the two questions drawing a row asks get answered — issue #123.
///
/// **The same answer, found two ways, because the workloads are opposite.**
/// `Reference::credits_on` and `Encodings::of` each walk their whole
/// collection. For one row that is right: a scan of 13,000 credits beats
/// building a map of them to ask once. `draw` asks about *every* row, and
/// there the scan was four passes over the credits and one over the encodings
/// per row — 333 ms of a 397 ms fold.
///
/// So the choice is made at the call site and named, rather than one strategy
/// being imposed on both. What is *not* duplicated is the answer: `line_of`
/// remains the only place that decides how a credit line reads.
enum Lookup<'a> {
    /// Ask the collections directly. For a caller drawing one row.
    Direct(&'a Librarian),
    /// Built once for a pass over the library.
    Built {
        l: &'a Librarian,
        credits: BTreeMap<CreditOn, Vec<&'a crate::model::reference::Credit>>,
        encodings: BTreeMap<Id<Recording>, Vec<&'a crate::model::encoding::Encoding>>,
    },
}

impl<'a> Lookup<'a> {
    /// The index `draw` uses, built once.
    fn built(l: &'a Librarian) -> Self {
        Lookup::Built {
            l,
            credits: l.reference.by_subject(),
            encodings: l.encodings.by_recording(),
        }
    }

    /// The credit line for a subject, as printed.
    ///
    /// Both arms end at `line_of`, which stays the only place that decides how
    /// a credit line reads. What differs is only how the credits were found.
    fn line(&self, subject: CreditOn) -> String {
        match self {
            Lookup::Direct(l) => l.reference.credited_as(subject),
            Lookup::Built { l, credits, .. } => credits
                .get(&subject)
                .map(|found| l.reference.line_of(found))
                .unwrap_or_default(),
        }
    }

    /// **Who is credited here, as ids** — #179, normalised by #193.
    ///
    /// Through the built index like everything else in this type, so a row
    /// costs a map lookup rather than a walk of every credit in the library —
    /// which is the four-passes-per-row shape #123 took out and which a
    /// per-row name assembly would have put straight back.
    fn credited_artists(&self, subject: CreditOn) -> Vec<Id<crate::model::entity::Artist>> {
        let credits: Vec<_> = match self {
            Lookup::Direct(l) => l.reference.credits_on(subject),
            Lookup::Built { credits, .. } => credits.get(&subject).cloned().unwrap_or_default(),
        };
        credits.into_iter().map(|c| c.artist.id()).collect()
    }

    /// The same line resolved through the artist, for a caller that groups.
    ///
    /// **Not the printed one, which is what `files_under` used to ask for**
    /// (#173). The two differ on exactly the credits that carry an
    /// `as_credited`, and those are the ones where a printed form would put
    /// one artist in two folders.
    ///
    /// `at` climbs the moniker graph before naming anybody — `None` is the
    /// artist credited and `Some(1)` the act behind it (#174).
    fn filing(&self, subject: CreditOn, at: Option<u8>) -> String {
        match (self, at) {
            (Lookup::Direct(l), None) => l.reference.files_under(subject),
            (Lookup::Direct(l), Some(at)) => l.reference.files_under_at(subject, at),
            (Lookup::Built { l, credits, .. }, at) => credits
                .get(&subject)
                .map(|found| match at {
                    None => l.reference.filed_under_of(found),
                    Some(at) => l.reference.filed_under_of_at(found, at),
                })
                .unwrap_or_default(),
        }
    }

    /// Whether the library holds any encoding of this recording.
    fn holds(&self, recording: Id<Recording>) -> bool {
        match self {
            Lookup::Direct(l) => !l.encodings.of(recording).is_empty(),
            Lookup::Built { encodings, .. } => encodings.contains_key(&recording),
        }
    }
}

/// Whether `long` is `short` plus a parenthetical — `Innocent When You Dream
/// (78)` over `Innocent When You Dream`.
///
/// A suffix in brackets is a qualifier: which take, which mix, which of two
/// same-named recordings on the same record. Anything else that merely starts
/// with the other title is a different song — `Home` and `Home Altars of
/// Mexico` are not one thing said twice — so the bracket is required, not
/// incidental.
fn qualifies(long: &str, short: &str) -> bool {
    long.len() > short.len()
        && long.starts_with(short)
        && long[short.len()..].trim_start().starts_with(['(', '['])
}

/// **What each artist is called** — #193.
///
/// The other half of normalising the row: a row holds ids and this answers
/// with names, so the name lives in one place and cannot disagree with a copy
/// of itself. Follows `Catalogue`'s `Listened` impl — the model supplying a
/// capability the query layer asks for, rather than the query layer reaching
/// into the model.
impl crate::query::filter::Named for Librarian {
    fn names_of(&self, artist: Id<crate::model::entity::Artist>) -> Vec<String> {
        self.reference.names_of(artist)
    }
}

impl Librarian {
    pub fn new() -> Self {
        Librarian {
            spine: Spine::default(),
            reference: Reference::default(),
            vocabulary: Vocabulary::default(),
            encodings: Encodings::default(),
            provenance: Provenance::default(),
            ratings: Default::default(),
            plays: Default::default(),
            concepts: Default::default(),
            described_by: Default::default(),
            rows: Default::default(),
            locators: Default::default(),
            held_at: Default::default(),
            collections: Default::default(),
            claims: Default::default(),
            identified: Default::default(),
            merged: Default::default(),
            encoding_derived: Default::default(),
            held: Default::default(),
            record_count: 0,
            collected: Default::default(),
            placed: Default::default(),
            placed_on: Default::default(),
        }
    }

    /// What a row on screen needs to draw itself.
    ///
    /// Assembled **by the library** out of five entities the player never has
    /// to know are separate — and carrying no path, because there is no path
    /// that is true of the library rather than of one machine.
    /// **What the library says about one row, borrowed from the fold** — #261.
    ///
    /// The hot half. `query::found` reads one per row of the whole library on every keystroke
    /// of a picker (0.65 ms over 7,273) and `Shelf::grouped` reads one per track; owning those
    /// would cost 3.0 ms apiece for a row that is read once and dropped.
    ///
    /// `Listing::listed` is the other half, and answers a [`crate::model::rows::Slip`] — the
    /// same row written out, for a reader with no fold to borrow from.
    pub fn describe(
        &self,
        id: crate::model::rows::CopyId,
    ) -> Option<crate::model::rows::Listed<'_>> {
        Some(self.rows.get(&id)?.listed())
    }

    pub fn row(&self, recording: Id<Recording>) -> Option<Row> {
        let canonical = self.encodings.canonical(recording).map(|e| e.id);
        self.row_of(recording, canonical)
    }

    /// The row for **one copy** — issue #78, and the question it settles.
    ///
    /// Measured on the real library: asked of a recording, 163 files showed an
    /// album that was not the one their own tags name. Asked of the copy, 15
    /// do, and every one of those is a title the model has settled the case of
    /// — `Strange Time To Be Alive` against `Strange Time to Be Alive` — which
    /// is one album having one name rather than a placement being wrong.
    ///
    /// A recording is on every release that holds it; a file is on one. Where
    /// the row is about a file, say which, and the album stops being a choice.
    /// `row` is this with the canonical copy filled in, which makes the
    /// answer a *rule* rather than whichever placement sorted first.
    pub fn row_of(&self, recording: Id<Recording>, copy: Option<Id<Encoding>>) -> Option<Row> {
        // Asked about a *file*, so the slot is the one that file fills.
        let slot = copy.and_then(|id| self.placed.get(&id).copied());
        self.row_of_via(recording, slot, copy, &Lookup::Direct(self))
    }

    /// [`Self::row_of`], against a credit index the caller already built.
    fn row_of_via(
        &self,
        recording: Id<Recording>,
        // **The appearance, given rather than derived from the copy** (#155).
        // It was read off `placed` for whichever file was in hand, so a slot
        // nothing fills had no album — and a track you do not own would be
        // drawn under `No Album` instead of the record it is missing from.
        slot: Option<Id<Track>>,
        copy: Option<Id<Encoding>>,
        by: &Lookup<'_>,
    ) -> Option<Row> {
        let rec = self.spine.recordings.get(&recording)?;
        let canonical = copy.and_then(|id| self.encodings.encodings.get(&id));
        Some(Row {
            recording,
            // **What the sleeve prints here, where it prints something else.**
            //
            // A recording is called one thing and a release may print another:
            // MusicBrainz's recording is `The Fox` and *Live 1975-85* prints
            // `The Fox (live)`; the recording is `Phobos` and *Luna* prints
            // `Phobos (Synthapella)`. `Tracklisted` has recorded that
            // difference since #310 and nothing read it, so cataloguing 487
            // tracklists would have stripped `(live)`, `(acoustic)` and
            // `(Synthapella)` off 87 files — the recording's name is right and
            // it is not the name of the thing in that slot.
            //
            // It cuts both ways, so neither name simply wins. *Beautiful
            // Maladies* prints `Innocent When You Dream` for the recording
            // `Innocent When You Dream (78)` — *Frank's Wild Years* carries
            // both the `(78)` and the `(Barroom)`, and the compilation, having
            // room for only one, drops the qualifier the recording needs. The
            // sleeve is shorter there and longer on *Live 1975-85*.
            //
            // So: whichever name carries the qualifier. Where one title is the
            // other plus a parenthetical, the parenthetical is information and
            // the qualified name is the more precise of the two; where they
            // differ any other way the sleeve is describing its own disc and
            // is right about it.
            //
            // Only where the copy actually sits on that release. A loose
            // recording, or one drawn outside any appearance, has no sleeve to
            // read and takes its own name.
            title: slot
                .and_then(|t| self.spine.tracks.get(&t))
                .and_then(|t| t.title_on_release.as_deref())
                .map(|sleeve| {
                    if qualifies(&rec.title, sleeve) {
                        rec.title.clone()
                    } else {
                        sleeve.to_owned()
                    }
                })
                .unwrap_or_else(|| rec.title.clone()),
            file_under: self.files_under(recording, slot, by),
            album: self.album_of(slot),
            // **The length of the copy this would open**, not of the
            // recording — a recording has no single length. An edit, a radio
            // cut and a truncated rip are one recording and three durations,
            // and "did I hear enough of it" is a question about the file that
            // played. Ranking them by length was a rule invented to fill a
            // field that should not have been filled: under it, a twelve-second
            // rip measured against a four-minute sibling could never earn a
            // play at all.
            length: canonical.map(|e| e.length).unwrap_or_default(),
            rating: self.ratings.get(&recording).copied(),
            plays: self.plays.get(&recording).copied().unwrap_or(0),
            format: canonical.map(|e| e.format),
            bitrate: canonical.map(|e| e.bitrate),
            verdict: canonical.map(|e| e.verdict()),
            available: by.holds(recording),
        })
    }

    /// Who a copy files under.
    ///
    /// The album-level credit first, then the recording's own — which is the
    /// modelled version of `Track::file_under`'s "album artist, else artist".
    /// The heuristic was guessing at a fact; this asks for it.
    ///
    /// **Asked of a slot, not of a recording.** It used to take the first of
    /// `tracks_of_recording`, and a recording is on every release that holds
    /// it — so a song on a compilation and its original album filed under
    /// whichever sorted first. On this library that split `Luna` across two
    /// artists with every tag on the album identical (#84).
    fn files_under(
        &self,
        recording: Id<Recording>,
        slot: Option<Id<Track>>,
        by: &Lookup<'_>,
    ) -> String {
        self.files_under_at(recording, slot, None, by)
    }

    /// [`Librarian::files_under`], optionally climbed to the act behind each
    /// moniker — #174.
    ///
    /// **The level belongs here rather than in the tree** because the answer
    /// is a credit line, and the one place that decides how a credit line
    /// reads is `line_of`. A tree that resolved artists itself and joined
    /// their names would be a second such place, differing from the first the
    /// first time a join phrase mattered.
    pub fn filed_under_at(
        &self,
        recording: Id<Recording>,
        slot: Option<Id<Track>>,
        at: u8,
    ) -> String {
        self.files_under_at(recording, slot, Some(at), &Lookup::Direct(self))
    }

    fn files_under_at(
        &self,
        recording: Id<Recording>,
        slot: Option<Id<Track>>,
        at: Option<u8>,
        by: &Lookup<'_>,
    ) -> String {
        let on_release = slot
            .and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| self.spine.media.get(&t.medium.id()))
            .and_then(|m| self.spine.releases.get(&m.release.id()))
            .map(|r| CreditOn::ReleaseGroup(r.group.id()));

        // **`filing`, not `line`** — #173. A folder is grouping, and the
        // printed form splits an artist that releases under a moniker.
        let filing = |subject: CreditOn| by.filing(subject, at);
        on_release
            .map(filing)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let by_recording = filing(CreditOn::Recording(recording));
                (!by_recording.is_empty()).then_some(by_recording)
            })
            .unwrap_or_else(|| UNKNOWN_ARTIST.to_string())
    }

    /// The album a **copy** files under, by way of the release its slot is on.
    ///
    /// See [`Librarian::files_under`] for why this is asked of a slot.
    fn album_of(&self, slot: Option<Id<Track>>) -> Option<String> {
        let track = self.spine.tracks.get(&slot?)?;
        let medium = self.spine.media.get(&track.medium.id())?;
        let release = self.spine.releases.get(&medium.release.id())?;
        self.spine
            .groups
            .get(&release.group.id())
            .map(|g| g.title.clone())
    }

    /// Which copy this machine should open, and where it sits on it.
    ///
    /// **The one place a locator is produced**, and it is produced at the
    /// moment of playing rather than stored anywhere the player can keep. A
    /// player that cannot hold a path cannot key anything by one.
    pub fn open(
        &self,
        recording: Id<Recording>,
        here: &[Id<Device>],
        prefers: &[Format],
    ) -> Option<(&Encoding, &str)> {
        let pinned = self
            .spine
            .recordings
            .get(&recording)
            .and_then(|r| r.pinned)
            .map(|p| p.id());
        let chosen = self
            .encodings
            .for_playback(recording, pinned, here, prefers)?;
        let locator = self
            .encodings
            .holders(chosen.id)
            .into_iter()
            .find(|h| here.contains(&h.device.id()))
            .map(|h| h.locator.as_str())?;
        Some((chosen, locator))
    }

    /// Build the library a log describes.
    ///
    /// The link that makes the whole layer end-to-end: files become a scan
    /// (#70), a scan becomes records (#69), records fold to a catalogue, and a
    /// catalogue becomes this. Nothing in the chain is a stored projection, so
    /// there is nothing that can drift out of step with the record of how it
    /// got there.
    ///
    /// Deliberately total. A encoding bound to a recording that no record
    /// catalogued still lands — that is the unidentified 7.3%, and it must not
    /// be second class.
    pub fn from_catalogue(cat: &Catalogue) -> Self {
        use crate::model::encoding::Encoding;
        use crate::model::spine::Recording as Rec;

        let mut l = Librarian::new();

        // How long each recording plays, taken from an encoding of it.
        //
        for (id, r) in &cat.recordings {
            l.spine.recordings.insert(
                *id,
                Rec {
                    id: *id,
                    title: r.title.clone(),
                    // A fold, not a field. A medley performs several works and
                    // the spine holds one, so an ambiguous answer is no answer
                    // — the second row is still in the catalogue for anything
                    // that can read a set.
                    work: match cat
                        .performances
                        .iter()
                        .filter(|(rec, _)| rec == id)
                        .collect::<Vec<_>>()
                        .as_slice()
                    {
                        [(_, w)] => Some(crate::model::id::Ref::to(*w)),
                        _ => None,
                    },
                    recorded: None,
                    bpm: None,
                    // Not filled: a recording has no length of its own.
                    // Its encodings do, and `Row` takes the one it would play.
                    length: std::time::Duration::ZERO,
                    pinned: None,
                },
            );
            l.reference.artists.entry(r.artist).or_insert_with(|| {
                crate::model::reference::ArtistRecord {
                    id: r.artist,
                    name: cat.artist_names.get(&r.artist).cloned().unwrap_or_default(),
                    sort_name: cat.artist_names.get(&r.artist).cloned().unwrap_or_default(),
                }
            });
        }
        // The role codelist and the credits, read.
        //
        // Both used to be made up here: one role minted from `b"performer"`,
        // and a credit pushed for every recording naming it. Every artist in
        // the library was a performer because one line said so, and a row that
        // could only say one thing said nothing.
        for (id, (name, class)) in &cat.roles {
            l.reference.roles.insert(
                *id,
                crate::model::reference::RoleRecord {
                    id: *id,
                    name: name.clone(),
                    class: *class,
                },
            );
        }
        for ((subject, artist, role, basis), c) in &cat.credits {
            l.reference.credits.push(crate::model::reference::Credit {
                subject: *subject,
                artist: crate::model::id::Ref::to(*artist),
                role: crate::model::id::Ref::to(*role),
                position: c.position,
                as_credited: c.as_credited.clone(),
                join_phrase: c.join_phrase.clone(),
                basis: *basis,
            });
        }
        for (id, name) in &cat.label_names {
            l.reference.labels.insert(
                *id,
                crate::model::reference::LabelRecord {
                    id: *id,
                    name: name.clone(),
                },
            );
        }
        for (id, title) in &cat.works {
            l.spine.works.insert(
                *id,
                crate::model::spine::Work {
                    id: *id,
                    title: title.clone(),
                },
            );
        }

        // The album spine.
        for (group, g) in &cat.groups {
            l.spine.groups.insert(
                *group,
                crate::model::spine::ReleaseGroup {
                    id: *group,
                    title: g.title.clone(),
                    first_released: g.first_released,
                    kind: g.kind,
                    secondary: g.secondary.clone(),
                },
            );
            l.reference.artists.entry(g.artist).or_insert_with(|| {
                crate::model::reference::ArtistRecord {
                    id: g.artist,
                    name: cat.artist_names.get(&g.artist).cloned().unwrap_or_default(),
                    sort_name: cat.artist_names.get(&g.artist).cloned().unwrap_or_default(),
                }
            });
        }

        // Read, not invented. Each of these three used to be constructed here
        // from whatever was to hand — with no stamp, no author and no way to
        // correct one — and looked exactly like a folded fact downstream.
        for (id, r) in &cat.releases {
            l.spine.releases.insert(
                *id,
                crate::model::spine::Release {
                    id: *id,
                    group: crate::model::id::Ref::to(r.group),
                    title: r.title.clone(),
                    released: r.released,
                    label: r.label.map(crate::model::id::Ref::to),
                    media: r.media,
                    catalogue_number: None,
                },
            );
        }
        for (id, m) in &cat.media {
            l.spine.media.insert(
                *id,
                crate::model::spine::Medium {
                    id: *id,
                    release: crate::model::id::Ref::to(m.release),
                    position: m.position,
                    format: m.format.clone(),
                    tracks: m.tracks,
                },
            );
        }
        for (id, p) in &cat.tracks {
            l.spine.tracks.insert(
                *id,
                crate::model::spine::Track {
                    id: *id,
                    medium: crate::model::id::Ref::to(p.medium),
                    recording: crate::model::id::Ref::to(p.recording),
                    position: p.position,
                    title_on_release: p.title_on_release.clone(),
                },
            );
        }

        // **A retired copy fills no slot** (#159). `placed` is what
        // `held_index` counts and what #155's "a disc nothing came off" rule
        // reads, so leaving a retired copy in it keeps a retired disc alive:
        // `Early James` had two of twelve still placed on the folder that went
        // to `.departed`, and those two kept the other ten drawing.
        l.placed = cat
            .placed
            .iter()
            .filter(|(id, _)| !cat.retired.contains(id))
            .map(|(id, slot)| (*id, *slot))
            .collect();
        l.reindex_placed();
        l.held = held_index(&l.spine, &l.placed);

        // Names are a register: whatever the fold settled on is the label.
        //
        // **Inserted, not merely renamed** — #152. An artist entered this map
        // only as a recording's or a group's *primary* artist, so one that
        // appears only in a **credit** had no entry and `line_of` resolved its
        // name to nothing. A credit line is built from names, so the whole
        // line came out empty and `file_under` fell back to `Unknown Artist`:
        // splitting the joined album credits on this library proposed moving
        // 288 files there, which is how this was found.
        for (id, name) in &cat.artist_names {
            let a = l.reference.artists.entry(*id).or_insert_with(|| {
                crate::model::reference::ArtistRecord {
                    id: *id,
                    name: String::new(),
                    sort_name: String::new(),
                }
            });
            a.name = name.clone();
            a.sort_name = name.clone();
        }
        // **The names, which never crossed before** — #179.
        //
        // `cat.aliases` has held them since the fold learned `AliasAsserted`,
        // and `Reference::aliases` has been the field they belong in, and
        // nothing carried one to the other. So every reader below this line
        // saw a library with no aliases in it: `artist_named` resolved only
        // by artists' own names, `shelf names` printed none, and the single
        // alias this library holds — the `Campfire Songs` an Animal
        // Collective merge retired — was unreachable from anywhere.
        //
        // The basis is `Manual`: a row that reached the catalogue was written
        // by something, and the fold does not keep which rung. Weakening it to
        // `Path` would let a re-import outrank a name a person recorded.
        for (subject, value, kind) in &cat.aliases {
            l.reference.aliases.push(crate::model::reference::Alias {
                subject: *subject,
                value: value.clone(),
                kind: *kind,
                basis: crate::model::reference::Source::Manual,
            });
        }
        // **The artist graph, kept as edges** — #174. Both ends stay rows;
        // only a level collapses them, so nothing here redirects.
        for (from, to, kind) in &cat.artist_relations {
            l.reference
                .artist_relations
                .push(crate::model::reference::ArtistRelation {
                    from: crate::model::id::Ref::to(*from),
                    to: crate::model::id::Ref::to(*to),
                    kind: *kind,
                    period: cat
                        .relation_periods
                        .get(&(*from, *to, *kind))
                        .map(|(began, ended)| (Some(*began), *ended)),
                });
        }
        // **A retired copy is not loaded** — #159, and this is the one place
        // it has to be excluded rather than everywhere downstream.
        //
        // The row is still in the log and the retirement is one fact to undo,
        // which is what makes `remove` reversible. What must not happen is the
        // library going on offering a copy it retired: `holds` would say yes,
        // the row would draw ungreyed, and `shelf render` would want to put
        // bytes somewhere they no longer are.
        for (id, d) in cat
            .encodings
            .iter()
            .filter(|(id, _)| !cat.retired.contains(id))
        {
            l.encodings.encodings.insert(
                *id,
                Encoding {
                    id: *id,
                    audio: d.audio,
                    recording: d.recording.map(crate::model::id::Ref::to),
                    format: d.format.unwrap_or(Format::Mp3),
                    bitrate: d.bitrate,
                    sample_rate: d.sample_rate,
                    channels: d.channels,
                    length: std::time::Duration::from_millis(u64::from(d.length_ms)),
                    processed: cat.processed.contains_key(id),
                    cutoff_hz: cat
                        .measurements
                        .get(id)
                        .map(|m| crate::model::encoding::Measured {
                            value: m.cutoff_hz,
                            analyser: m.analyser,
                        }),
                    cliff_db: cat.measurements.get(id).and_then(|m| {
                        m.cliff_db.map(|db| crate::model::encoding::Measured {
                            value: db,
                            analyser: m.analyser,
                        })
                    }),
                    gain: None,
                    levels: cat.levels.get(id).copied().unwrap_or_default(),
                    encoder_delay: None,
                },
            );
        }
        for (device, asserted) in &cat.holdings {
            for (dist, locator) in &asserted.held {
                l.encodings.holdings.push(crate::model::encoding::Holding {
                    encoding: crate::model::id::Ref::to(*dist),
                    device: crate::model::id::Ref::to(*device),
                    locator: locator.clone(),
                });
            }
        }
        l.ratings = cat.ratings.clone();
        l.identified = cat.identified.clone();
        l.merged = cat.merges.redirects().collect();
        l.encoding_derived = cat.encoding_derived.iter().copied().collect();
        l.plays = cat.plays.clone();

        // **One pass over the slots, not one per row** — #253. Every recording's
        // groups, with compilations kept only as a last resort.
        {
            // **Best-so-far, not collect-then-sort.** The first version built a `Vec`
            // per recording and sorted each one, which cost 1.9 ms of a 24.5 ms fold —
            // measurable, and unnecessary: the answer is a minimum, and a minimum needs
            // no list. `(false, _)` sorts before `(true, _)`, so a non-compilation
            // wins and the id breaks the remaining tie identically every time.
            // **Only recordings on more than one group get an entry.** For everything
            // else the answer is the group of the slot being drawn, which is what the
            // fallback already uses — so storing it would be a map the size of the
            // library restating what the caller has in hand. This one holds the
            // handful of recordings held on an album *and* a compilation, which is the
            // only case the rule changes.
            let mut best: BTreeMap<Id<Recording>, (bool, Id<ReleaseGroup>, bool)> = BTreeMap::new();
            for track in l.spine.tracks.values() {
                let Some(medium) = l.spine.media.get(&track.medium.id()) else {
                    continue;
                };
                let Some(release) = l.spine.releases.get(&medium.release.id()) else {
                    continue;
                };
                let group = release.group.id();
                let compilation = l
                    .spine
                    .groups
                    .get(&group)
                    .is_some_and(|g| g.is_compilation());
                let candidate = (compilation, group);
                best.entry(track.recording.id())
                    .and_modify(|held| {
                        if held.1 != group {
                            held.2 = true;
                        }
                        if candidate < (held.0, held.1) {
                            (held.0, held.1) = candidate;
                        }
                    })
                    .or_insert((compilation, group, false));
            }
            l.described_by = best
                .into_iter()
                .filter(|(_, (_, _, several))| *several)
                .map(|(recording, (_, group, _))| (recording, group))
                .collect();
        }
        l.collections = cat.collections.clone();
        l.collected = cat.entries.clone();

        // **The hierarchy, which nothing carried across before** (#116). The
        // fold has held these edges since #74 and the librarian left them
        // behind, so `depth` had nothing to walk and the genres on a row could
        // only be ordered by name.
        for (hierarchy, (_, name)) in &cat.hierarchies {
            l.vocabulary.hierarchies.insert(
                *hierarchy,
                crate::model::vocabulary::HierarchyRecord {
                    id: *hierarchy,
                    name: name.clone(),
                },
            );
        }
        // **And the terms, which it left behind the same way** (#79). Edges
        // without labels let `depth` walk but gave `at_level` an ancestor it
        // could only name by falling back to the concept it started from — so
        // `levels genre@1` silently drew the specific term as the broad one.
        for (id, record) in &cat.concepts {
            l.vocabulary.genres.insert(
                *id,
                crate::model::vocabulary::Term {
                    id: *id,
                    name: record.label.clone(),
                },
            );
        }
        for (hierarchy, parent, child) in &cat.narrows {
            l.vocabulary
                .edges
                .insert(crate::model::vocabulary::HierarchyEdge {
                    hierarchy: crate::model::id::Ref::to(*hierarchy),
                    parent: crate::model::id::Ref::to(*parent),
                    child: crate::model::id::Ref::to(*child),
                });
        }

        // Every genre a subject is assigned, most confident first. Several
        // sources may assign the same one — a tag and a curated pass agreeing
        // is two rows in the fold and one genre on the shelf — so the best
        // confidence for each name wins and the name is listed once.
        // **Keyed by axis, not one map per axis** (#117), so an axis added to
        // `facet::ALL` crosses the seam with nothing written here. A node
        // label is left out: ISO 25964's rule is that a term put into an array
        // to organise it does not take part in retrieval.
        let mut depths: BTreeMap<Id<crate::model::entity::Concept>, usize> = BTreeMap::new();
        let mut best: BTreeMap<(Described, Id<crate::model::entity::Facet>), Ranked<'_>> =
            BTreeMap::new();
        for ((subject, concept, basis), confidence) in &cat.assertions {
            let retracted = cat.assertions_retracted.get(&(*subject, *concept)).copied();
            let Some(record) = cat.concepts.get(concept).filter(|c| !c.node_label) else {
                continue;
            };
            // **Every claim crosses, retracted or not** (#125). The collapsed
            // `concepts` below is what a row draws; this is what the info
            // panel reads, and it is the only place the basis survives.
            l.claims
                .entry((*subject, record.facet))
                .or_default()
                .push(Claimed {
                    label: record.label.clone(),
                    said: Some((*basis, *confidence)),
                    retracted,
                });
            if retracted.is_some() {
                continue;
            }
            let name = &record.label;
            // **Memoised, because a vocabulary is small and the assertions
            // are not** (#123). `depth` walks the edge set once per hop, and
            // this library asks it 26,490 times about roughly 250 distinct
            // concepts.
            let depth = *depths
                .entry(*concept)
                .or_insert_with(|| l.vocabulary.depth(*concept));
            let seen = best.entry((*subject, record.facet)).or_default();
            match seen.iter_mut().find(|(_, _, held)| *held == name.as_str()) {
                Some(entry) if entry.0 < *confidence => entry.0 = *confidence,
                Some(_) => {}
                None => seen.push((*confidence, depth, name.as_str())),
            }
        }
        // **What was ruled out, and nothing asserts any more** (#125). The
        // fold sweeps a shadowed assertion out of `assertions` entirely, so
        // these have no row above to carry them — and leaving them out would
        // make "somebody decided against this" look like "nobody said it".
        for ((subject, concept), by) in &cat.assertions_retracted {
            let Some(record) = cat.concepts.get(concept) else {
                continue;
            };
            let held = l.claims.entry((*subject, record.facet)).or_default();
            if !held.iter().any(|c| c.label == record.label) {
                held.push(Claimed {
                    label: record.label.clone(),
                    said: None,
                    retracted: Some(*by),
                });
            }
        }

        l.concepts = best
            .into_iter()
            .map(|(subject, mut names)| {
                // Most confident, then **broadest**, then by name.
                //
                // **Tag order does not survive the fold and is not meant to.**
                // `Rock; Blues Rock; Alternative Rock` arrives broadest-first
                // because that is how taggers write it, but the catalogue keys
                // assignments by `(subject, genre, source)` — a set, with no
                // position, because a position is a fact about a *string* and
                // not about the genres.
                //
                // The hierarchy is what orders them: `Rock` is a top concept
                // and `Alternative Rock` is narrower-of it, so depth answers
                // "which is broadest" from the data rather than from the
                // spelling. Where no edge places a genre it is depth 0 and the
                // name breaks the tie, which is the alphabetical order this
                // had before any hierarchy existed (#116).
                names.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
                (
                    subject,
                    names.into_iter().map(|(_, _, n)| n.to_string()).collect(),
                )
            })
            .collect();

        l.draw();
        l
    }

    /// Every recorded fact naming this thing, newest first — #128.
    ///
    /// **Filtered on the fact's own serialization**, which is why there is no
    /// per-variant match here to fall out of step. A fact mentions a focus if
    /// its wire form carries that id, and that is true of every variant there
    /// is and every variant there will be — which is the same rule #106 asks a
    /// stale *reader* to follow, applied to a reader of the log.
    /// **Over records handed in, not records held** — #256.
    ///
    /// This read `self.records`, which is why every fold carried a clone of the whole
    /// log — 75.5 MB of the 102 MB a fold of the real library kept, for a surface opened
    /// deliberately and cached per focus. The librarian thread owns the log and can scan
    /// it for one focus in answer to a question; `Desk::history` is that question.
    pub fn facts_about(
        &self,
        records: &std::collections::BTreeSet<crate::facts::Record>,
        focus: crate::model::focus::Focus,
    ) -> Vec<Happened> {
        let id = focus.uuid().to_string();
        let mut out: Vec<Happened> = records
            .iter()
            .filter_map(|r| {
                let wire = serde_json::to_value(&r.fact).ok()?;
                let (name, fields) = wire.as_object()?.iter().next()?;
                if !wire.to_string().contains(&id) {
                    return None;
                }
                Some(Happened {
                    at: r.stamp.wall,
                    device: r.stamp.device.to_string(),
                    name: name.clone(),
                    detail: self.detail_of(fields),
                    withdrawal: is_withdrawal(name),
                })
            })
            .collect();
        // Newest first: what changed recently is what somebody is looking for.
        out.sort_by_key(|h| std::cmp::Reverse(h.at));
        out
    }

    /// One fact's fields, with ids resolved to names where the library can.
    ///
    /// **No per-variant knowledge, only a lookup.** Any uuid the library can
    /// put a name to becomes that name; anything else is printed as it is. A
    /// fact this build has never heard of still reads.
    fn detail_of(&self, fields: &serde_json::Value) -> String {
        let Some(fields) = fields.as_object() else {
            return fields.to_string();
        };
        fields
            .iter()
            .map(|(key, value)| {
                let shown = match value.as_str().and_then(|s| s.parse::<uuid::Uuid>().ok()) {
                    Some(id) => self.name_of(id).unwrap_or_else(|| value.to_string()),
                    None => match value {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    },
                };
                format!("{key}={shown}")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What the library calls this id, whatever kind it turns out to be.
    fn name_of(&self, id: uuid::Uuid) -> Option<String> {
        use crate::model::focus::Focus;
        [
            Focus::Recording(Id::from_uuid(id)),
            Focus::Concept(Id::from_uuid(id)),
            Focus::Artist(Id::from_uuid(id)),
            Focus::Collection(Id::from_uuid(id)),
            Focus::ReleaseGroup(Id::from_uuid(id)),
            Focus::Release(Id::from_uuid(id)),
            Focus::Work(Id::from_uuid(id)),
        ]
        .into_iter()
        .map(|focus| crate::model::about::label_of(self, focus))
        .find(|name| name != "(not in the library)")
    }

    /// The questions the info panel asks about one focus — #125.
    ///
    /// **Scans, and that is right here.** #121 and #123 were the same mistake
    /// twice: a linear walk, correct for one question, run once per row. These
    /// answer about *one* thing when a panel opens, so walking is the cheap
    /// way and an index would be the expensive one.
    pub fn claims_on(
        &self,
        subject: Described,
        facet: Id<crate::model::entity::Facet>,
    ) -> &[Claimed] {
        self.claims
            .get(&(subject, facet))
            .map_or(&[][..], Vec::as_slice)
    }

    /// What this file was made out of, if anything here was — #132.
    ///
    /// `None` means **not known to be regenerable**, which is the answer that
    /// must be safe to act on: an acquired file and one whose source has been
    /// forgotten look the same from here, and both need keeping.
    pub fn derived_from(&self, copy: Id<Encoding>) -> Option<Id<Encoding>> {
        self.encoding_derived
            .iter()
            .find(|(_, to)| *to == copy)
            .map(|(from, _)| *from)
    }

    /// What was made out of this file.
    pub fn derived_copies(&self, copy: Id<Encoding>) -> Vec<Id<Encoding>> {
        self.encoding_derived
            .iter()
            .filter(|(from, _)| *from == copy)
            .map(|(_, to)| *to)
            .collect()
    }

    /// What a recording's identity rests on, said as a person would read it.
    pub fn identified_by(&self, recording: Id<Recording>) -> Option<String> {
        self.identified.get(&recording).map(|b| format!("{b:?}"))
    }

    /// What was folded into this recording.
    pub fn merged_into(&self, recording: Id<Recording>) -> Vec<Id<Recording>> {
        self.merged
            .iter()
            .filter(|(_, now)| *now == recording)
            .map(|(was, _)| *was)
            .collect()
    }

    /// What a credit's role is called.
    pub fn role_of(&self, role: Id<crate::model::entity::Role>) -> String {
        self.reference
            .roles
            .get(&role)
            .map_or_else(|| "credit".to_string(), |r| r.name.clone())
    }

    /// Where this recording sits on a pressing, if anywhere.
    pub fn slots_of(&self, recording: Id<Recording>) -> Vec<&crate::model::spine::Track> {
        self.spine
            .tracks
            .values()
            .filter(|t| t.recording.id() == recording)
            .collect()
    }

    /// **Whether the library knows these bytes at all** — #140.
    ///
    /// A scan asks it of everything it walked past: what the library does not
    /// know is not a row, it is an offer to `import`.
    pub fn holds_encoding(&self, copy: Id<Encoding>) -> bool {
        self.encodings.encodings.contains_key(&copy)
    }

    /// Where this copy is on *this* device, if it is here at all.
    pub fn where_held(&self, copy: Id<Encoding>) -> Option<String> {
        self.locators.get(&copy).cloned()
    }

    /// Every device holding this copy, and where.
    pub fn holdings_of(&self, copy: Id<Encoding>) -> Vec<(String, String)> {
        self.encodings
            .holdings
            .iter()
            .filter(|h| h.encoding.id() == copy)
            .map(|h| (h.device.id().to_string(), h.locator.clone()))
            .collect()
    }

    /// **The recordings a focus stands for** — the grain a crate holds, and so the
    /// grain a page is edited at.
    ///
    /// A crate entry is a recording (`CollectionEntryAdded`), so filing an artist means
    /// filing each recording of theirs, and the artist's page has to say which crates
    /// hold *how many* of them rather than drawing a tick. This is the list both
    /// questions are asked of.
    ///
    /// An artist covers what is credited to them at any grain — a recording, or every
    /// recording on a release group or release credited to them — so a compilation
    /// track credited to the artist is theirs even though the tree files it under
    /// *Various Artists*. A concept covers nothing: a genre is asserted *of* rows and
    /// does not contain them, and a crate edit on one would be a query result
    /// mistaken for a thing.
    pub fn covers(&self, focus: crate::model::focus::Focus) -> Vec<Id<Recording>> {
        use crate::model::focus::Focus;
        let mut out: Vec<Id<Recording>> = match focus {
            Focus::Recording(r) => vec![r],
            Focus::Encoding(e) => self
                .encodings
                .encodings
                .get(&e)
                .and_then(|enc| enc.recording.as_ref())
                .map(|r| r.id())
                .into_iter()
                .collect(),
            Focus::Release(rel) => self
                .spine
                .tracks
                .values()
                .filter(|t| {
                    self.spine
                        .media
                        .get(&t.medium.id())
                        .is_some_and(|m| m.release.id() == rel)
                })
                .map(|t| t.recording.id())
                .collect(),
            Focus::ReleaseGroup(g) => self
                .spine
                .releases
                .values()
                .filter(|r| r.group.id() == g)
                .flat_map(|r| self.covers(Focus::Release(r.id)))
                .collect(),
            Focus::Work(w) => self
                .spine
                .recordings
                .values()
                .filter(|r| r.work.as_ref().is_some_and(|it| it.id() == w))
                .map(|r| r.id)
                .collect(),
            Focus::Artist(a) => self
                .reference
                .credits
                .iter()
                .filter(|c| c.artist.id() == a)
                .flat_map(|c| match c.subject {
                    CreditOn::Recording(r) => vec![r],
                    CreditOn::ReleaseGroup(g) => self.covers(Focus::ReleaseGroup(g)),
                    CreditOn::Release(rel) => self.covers(Focus::Release(rel)),
                    CreditOn::Work(w) => self.covers(Focus::Work(w)),
                })
                .collect(),
            Focus::Collection(c) => crate::model::rows::Listing::entries_of(self, c),
            Focus::Concept(_) => Vec::new(),
        };
        // **Distinct, in first-seen order.** A recording credited on both the record and
        // the track is one recording, and the count on a page is a count of those.
        let mut seen = BTreeSet::new();
        out.retain(|r| seen.insert(*r));
        out
    }

    /// The collections holding this recording.
    pub fn crates_holding(&self, recording: Id<Recording>) -> Vec<Id<Collection>> {
        let mut out: Vec<_> = self
            .collected
            .iter()
            .filter(|(_, _, r)| *r == recording)
            .map(|(c, _, _)| *c)
            .collect();
        out.dedup();
        out
    }

    /// How many subjects this concept is asserted of.
    pub fn asserting(&self, concept: Id<crate::model::entity::Concept>) -> usize {
        let name = self
            .vocabulary
            .genres
            .get(&concept)
            .map(|g| g.name.as_str())
            .unwrap_or_default();
        self.claims
            .values()
            .filter(|claims| {
                claims
                    .iter()
                    .any(|c| c.label == name && c.retracted.is_none())
            })
            .count()
    }

    /// The concepts directly broader than this one.
    pub fn broader_than(
        &self,
        concept: Id<crate::model::entity::Concept>,
    ) -> Vec<Id<crate::model::entity::Concept>> {
        self.vocabulary
            .edges
            .iter()
            .filter(|e| e.child.id() == concept)
            .map(|e| e.parent.id())
            .collect()
    }

    /// The recordings that realise this work.
    pub fn performances_of(&self, work: Id<crate::model::entity::Work>) -> Vec<Id<Recording>> {
        self.spine
            .recordings
            .values()
            .filter(|r| r.work.map(|w| w.id()) == Some(work))
            .map(|r| r.id)
            .collect()
    }

    /// Rebuild `placed_on` from `placed` — see the field. Called by
    /// `from_catalogue`, and by anything that writes `placed` by hand.
    pub fn reindex_placed(&mut self) {
        self.placed_on.clear();
        for (copy, slot) in &self.placed {
            self.placed_on.entry(*slot).or_default().push(*copy);
        }
    }

    /// Say which device this is, so rows know where they can be opened.
    pub fn here(&mut self, device: Id<Device>) {
        let mine = || {
            self.encodings
                .holdings
                .iter()
                .filter(|h| h.device.id() == device)
        };
        self.locators = mine()
            .map(|h| (h.encoding.id(), h.locator.clone()))
            .collect();
        self.held_at = mine()
            .map(|h| (h.locator.clone(), h.encoding.id()))
            .collect();
    }

    /// Materialise every row.
    ///
    /// One pass at the end of the fold rather than one assembly per draw. The
    /// UI redraws five times a second and a screenful is fifty rows; building
    /// them on demand would recompute the same credits and album titles
    /// hundreds of times a second for a library that changed between none of
    /// them.
    fn draw(&mut self) {
        // **Built once for the whole library** (#123). `credits_on` is a
        // linear scan, and asking it per row was 6,500 full passes over 13,000
        // credits — 333ms of a 397ms fold.
        use crate::model::rows::CopyId;
        let by = Lookup::built(self);
        // **One row per appearance, plus one per recording that has none** —
        // #155. It was one per *encoding*, so an album held as FLAC and MP3
        // drew every track twice, and a slot nothing fills could not be drawn.
        //
        // Both indexes are built once, for #123's reason: `draw` asks about
        // every row, and scanning `placed` per row is a full pass each time.
        let mut copies: std::collections::BTreeMap<Id<Track>, Vec<Id<Encoding>>> =
            Default::default();
        for (copy, slot) in &self.placed {
            copies.entry(*slot).or_default().push(*copy);
        }
        let appears: std::collections::BTreeSet<Id<Recording>> = self
            .spine
            .tracks
            .values()
            .map(|t| t.recording.id())
            .collect();

        // **A disc nothing here came off is not a record you are missing
        // tracks from** — #155, and the rule that stops 384 rows on this
        // library being drawn twice.
        //
        // A retag that changes an album's MusicBrainz id mints a new release
        // group, a new pressing, a new disc and new slots, and the copies move
        // to them. The old disc stays — the log is append-only and nothing
        // retracts a slot — and while a row was a *copy* it drew nothing.
        // Drawing appearances makes every one of them a second, empty row
        // beside the real one: `The Bug — London Zoo` had two release groups
        // of that name and showed track 11 twice, once with no length.
        //
        // The honest reading is that an empty slot only means something
        // against a record you own part of. Where the whole disc is empty, the
        // library is not short of a track — it is holding an identity that
        // moved.
        let lived_on: std::collections::BTreeSet<Id<crate::model::entity::Medium>> = self
            .placed
            .values()
            .filter_map(|slot| self.spine.tracks.get(slot))
            .map(|t| t.medium.id())
            .collect();
        // **An empty slot at a position something already fills is not a gap**
        // — #168.
        //
        // A slot is a row when a copy fills it, or when the library holds
        // something else off that medium — so a record you own eleven of
        // fifteen tracks of shows four gaps, which is #134's whole point. What
        // that rule cannot tell on its own is a gap from a *shadow*.
        //
        // `Id::<Track>::of_track` seeds a slot from medium and recording, so
        // two passes disagreeing about which recording sits at a position mint
        // two ids for it. A relink then re-points the one it finds, both come
        // to name the same performance, and the two ids remain: the same place
        // on the same disc, twice, one playable and one not. *21* drew ten of
        // its fifteen tracks that way. The same shape covers a position still
        // contested, where the claimants differ and one has the file — and
        // there too the position is not missing, it is disputed, which is
        // `shelf slots`' business and not a hole in the record.
        //
        // So: a slot with no copy, at a place another slot fills, is not
        // drawn. A position nothing fills still is, because that is the gap.
        let filled_at: std::collections::BTreeSet<(Id<crate::model::entity::Medium>, u16)> = self
            .spine
            .tracks
            .values()
            .filter(|t| copies.contains_key(&t.id))
            .map(|t| (t.medium.id(), t.position))
            .collect();
        let ids: Vec<CopyId> = self
            .spine
            .tracks
            .iter()
            .filter(|(id, t)| copies.contains_key(*id) || lived_on.contains(&t.medium.id()))
            .filter(|(id, t)| {
                copies.contains_key(*id) || !filled_at.contains(&(t.medium.id(), t.position))
            })
            .map(|(t, _)| CopyId::On(*t))
            // **A recording on no release is a row too**, which MusicBrainz
            // calls a standalone recording and this library has one of. Minting
            // it a release group to give it a home is what
            // `Various Artists/YouTube Sessions` already is (#20).
            //
            // **But something has to hold it** — #168. The slot branch above
            // already asks this: a slot is a row when a copy fills it, or when
            // the library holds *something* off that medium, so a part-held
            // album shows its gaps (#134) and an album nobody owns shows
            // nothing. A loose recording has no medium to be a gap in, and
            // without the same question it became a row on no evidence at all.
            //
            // What that let through is residue. An identification that moves —
            // `relink`, 377 of them here — leaves the recording it moved away
            // from holding no file and sitting in no slot, and 100 such rows
            // had accumulated, drawn beside the real ones and playable by
            // nothing. They are not standalone recordings; they are the shadow
            // of a claim somebody withdrew.
            //
            // Any copy, not a copy *here*: a recording held on another device
            // is an ordinary row and an unplayable one, which is a different
            // answer from "no such row".
            .chain(
                self.spine
                    .recordings
                    .keys()
                    .filter(|r| !appears.contains(r) && by.holds(**r))
                    .map(|r| CopyId::Loose(*r)),
            )
            .collect();
        let drawn: Vec<(CopyId, Drawn)> = ids
            .into_iter()
            .filter_map(|id| Some((id, self.drawn_of(id, &copies, &by)?)))
            .collect();
        self.rows.extend(drawn);
    }

    /// Assemble one row out of the entities behind it.
    fn drawn_of(
        &self,
        id: crate::model::rows::CopyId,
        copies: &std::collections::BTreeMap<Id<Track>, Vec<Id<Encoding>>>,
        by: &Lookup<'_>,
    ) -> Option<Drawn> {
        use crate::model::rows::CopyId;
        let (recording, slot) = match id {
            CopyId::On(track) => (self.spine.tracks.get(&track)?.recording.id(), Some(track)),
            CopyId::Loose(recording) => (recording, None),
        };
        // **The best copy of this appearance**, and `None` where nothing fills
        // it: a slot the library knows about and holds no file for. A loose
        // recording takes any copy of itself, since it has no appearance to be
        // a copy *of*.
        let shown = match slot {
            Some(track) => copies
                .get(&track)
                .into_iter()
                .flatten()
                .filter_map(|c| self.encodings.encodings.get(c))
                .max_by_key(|e| crate::model::encoding::canonical_key(e)),
            None => self.encodings.canonical(recording),
        };
        let copy = shown.map(|e| e.id);
        let row = self.row_of_via(recording, slot, copy, by)?;
        let (disc, track) = self.place_of(slot);
        let group = self.group_of(slot);

        // **Every concept on the recording**, per axis, and the album's only
        // where the recording carries none of its own — a compilation's `Pop`
        // must not join what the track itself was asserted (#74). The rule is
        // per axis rather than per row, so an album-level genre and a
        // track-level version do not shadow one another.
        let mut concepts: std::collections::BTreeMap<_, Vec<String>> =
            std::collections::BTreeMap::new();
        for facet in crate::model::facet::ALL
            .iter()
            .map(|(key, _)| Id::<crate::model::entity::Facet>::of_key(key))
        {
            let mut held = self
                .concepts
                .get(&(Described::Recording(recording), facet))
                .cloned()
                .unwrap_or_default();
            if held.is_empty() {
                // **The group that describes it, not the one being browsed** — #253.
                // `described_by` prefers a non-compilation, so a rock song reached
                // through _80s Hits_ keeps its album's genre rather than the
                // compilation's `Pop`. Falls back to this row's own group, which is
                // the answer whenever the recording sits on exactly one.
                held = self
                    .described_by
                    .get(&recording)
                    .copied()
                    .or(group)
                    .and_then(|g| self.concepts.get(&(Described::ReleaseGroup(g), facet)))
                    .cloned()
                    .unwrap_or_default();
            }
            if !held.is_empty() {
                concepts.insert(facet, held);
            }
        }

        let on_release = slot
            .and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| self.spine.media.get(&t.medium.id()))
            .and_then(|m| self.spine.releases.get(&m.release.id()));

        // **Through the same lookup as everything else here** (#123). These
        // two were the rest of the per-row scan: `files_under` above and these
        // below are four passes over every credit in the library, per row.
        let line = |subject: CreditOn| by.line(subject);

        // **Only the names this row does not already carry** — #179.
        //
        // An artist's own name is usually exactly what the row prints, so
        // keeping it would put a duplicate string on every row in the library
        // to serve the few that have a second name. Filtered against the
        // printed forms, this is empty for almost all of them.
        //
        // Normalised for the comparison, which is what `artist_named` resolves
        // by: a name differing only in case or marks is already carried.
        //
        // **Each credit line is formatted once and used twice.** `line_of`
        // joins credits into a string, and asking it again to find out what
        // the row prints would double that work on every row in the library.
        let credited = line(CreditOn::Recording(recording));
        let album_line = group.map(|g| line(CreditOn::ReleaseGroup(g)));

        // **Who the row credits, as ids** — #179 normalised by #193.
        //
        // No names and no filtering against what is printed, because nothing
        // is being stored that could disagree with anything: an id is the
        // artist, and what they are called is asked of the artist when
        // somebody needs to read it.
        let credits: Vec<Id<crate::model::entity::Artist>> = {
            let mut out: Vec<_> = std::iter::once(CreditOn::Recording(recording))
                .chain(group.map(CreditOn::ReleaseGroup))
                .flat_map(|subject| by.credited_artists(subject))
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        };

        Some(Drawn {
            file_under: row.file_under,
            album: row
                .album
                .clone()
                .unwrap_or_else(|| crate::model::tree::NO_ALBUM.to_string()),
            // `u16::MAX` is `place_of`'s "not numbered" — it sorts unplaced
            // copies last rather than first, which is why it is not zero. Read
            // as a number it shows a track 65535.
            disc: (disc != u16::MAX).then_some(u32::from(disc)),
            track: (track != u16::MAX).then_some(u32::from(track)),
            title: row.title.clone(),
            duration: row.length,
            artist: (!credited.is_empty()).then_some(credited),
            album_artist: album_line,
            // The model's title *is* the tagged one, normalised. There is no
            // file name to fall back to here, which is the point: a row holds
            // no location, so it cannot borrow one for a label.
            title_if_named: (!row.title.is_empty()).then(|| row.title.clone()),
            album_if_named: row.album,
            concepts,
            credited: credits,
            label: on_release
                .and_then(|r| r.label)
                .and_then(|l| self.reference.labels.get(&l.id()))
                .map(|l| l.name.clone()),
            // **Asked of the release group**, which is where the kind lives:
            // a reissue is a different pressing of the same record, and
            // whether that record is a single is not something a pressing
            // changes. A row on no release is an album by default — the same
            // default a file with no tag gets (#133).
            // **Asked of the pressing this copy sits on** (#134), which is
            // the thing that has a track count. The release group is the album
            // across its editions and a bonus disc belongs to one of them.
            release_held: on_release.map(|r| self.held_of(r.id)).unwrap_or_default(),
            release_type: group
                .and_then(|g| self.spine.groups.get(&g))
                .map(|g| g.kind)
                .unwrap_or_default(),
            release_secondary: group
                .and_then(|g| self.spine.groups.get(&g))
                .map(|g| g.secondary.clone())
                .unwrap_or_default(),
            // The album's first release where the tags say so, else this
            // pressing's. `OriginalReleaseDate` is the #59-correct answer — a
            // 2015 pressing of a 1982 album is 1982 — and 98% of this library
            // does not carry it, so falling back is the difference between a
            // year and a blank.
            year: group
                .and_then(|g| self.spine.groups.get(&g))
                .and_then(|g| g.first_released)
                .or_else(|| on_release.and_then(|r| r.released))
                .map(|d| i32::from(d.year)),
            // **Of the copy this row shows**, absent where it shows none: a
            // slot has no bitrate, a file does.
            bitrate: shown.map(|e| e.bitrate),
            // **What a `quality` pass found, where one has run.** `bitrate` is
            // what the file claims; this is where its spectrum actually stops,
            // so a 320 that came from a 128 shows up as the two disagreeing.
            cutoff_hz: shown.and_then(|e| e.cutoff_hz).map(|m| m.value),
            cliff_db: shown.and_then(|e| e.cliff_db).map(|m| m.value),
            // **From the copy this row is about, not the canonical one.** `Row.verdict`
            // asks the canonical encoding, which is right for "what is the best I hold";
            // a row drawn for one file is about *that* file, and `--every-copy` exists so
            // a person can ask per copy. Asking the canonical here would report a FLAC's
            // verdict on the MP3 beside it.
            verdict: shown.map(|e| e.verdict()),
            sample_rate: shown.and_then(|e| e.sample_rate),
            channels: shown.and_then(|e| e.channels),
            // **Of the recording, not of the slot** (#155). `shown` is empty
            // for a compilation's slot whose copy you own off the original
            // album, and that row plays perfectly well — `open` asks the
            // recording for a copy and gets one. What earns a greyed row is
            // holding no file for the performance at all.
            held: by.holds(recording),
        })
    }

    /// Every copy the library holds, whatever row shows it — #155.
    ///
    /// **Storage is copy-shaped and stays so.** A row is an appearance and an
    /// appearance is not a file; `shelf render` places bytes, so it asks for
    /// bytes.
    pub fn every_copy(&self) -> Vec<Id<Encoding>> {
        self.encodings.encodings.keys().copied().collect()
    }

    /// The row a copy belongs to — its appearance, or the loose recording it
    /// holds where no release does.
    pub fn row_of_copy(&self, copy: Id<Encoding>) -> Option<crate::model::rows::CopyId> {
        use crate::model::rows::CopyId;
        if let Some(track) = self.placed.get(&copy) {
            return Some(CopyId::On(*track));
        }
        Some(CopyId::Loose(
            self.encodings.encodings.get(&copy)?.recording?.id(),
        ))
    }

    /// The copy a row shows, if anything fills it — #155.
    pub fn shown_copy(&self, id: crate::model::rows::CopyId) -> Option<Id<Encoding>> {
        use crate::model::rows::CopyId;
        match id {
            CopyId::On(track) => self
                .placed_on
                .get(&track)
                .into_iter()
                .flatten()
                .filter_map(|c| self.encodings.encodings.get(c))
                .max_by_key(|e| crate::model::encoding::canonical_key(e))
                .map(|e| e.id),
            CopyId::Loose(recording) => self.encodings.canonical(recording).map(|e| e.id),
        }
    }

    /// **How much of this pressing is here** — #134.
    ///
    /// A lookup, because the walk behind it is done once per fold (#137). See
    /// [`held_index`] for what "complete" has to mean.
    pub fn held_of(&self, release: Id<crate::model::entity::Release>) -> Held {
        self.held
            .get(&release)
            .map(|c| c.answer)
            .unwrap_or_default()
    }

    /// **How many of how many** — the numbers behind [`Self::held_of`], #137.
    ///
    /// `None` when the total is not known, which is the same 440-file case
    /// `Held::Unknown` covers: there is no denominator to print.
    pub fn counted_of(&self, release: Id<crate::model::entity::Release>) -> Option<(usize, u16)> {
        let c = self.held.get(&release)?;
        Some((c.here, c.total?))
    }

    /// The slot a copy sits in, if it sits in one.
    pub fn placed_at(&self, copy: Id<Encoding>) -> Option<Id<Track>> {
        self.placed.get(&copy).copied()
    }

    /// One encoding, by id.
    pub fn encoding(&self, copy: Id<Encoding>) -> Option<&Encoding> {
        self.encodings.encodings.get(&copy)
    }

    /// How many discs a pressing has, where anything counted — #134.
    pub fn release_media(&self, release: Id<crate::model::entity::Release>) -> Option<u16> {
        self.spine.releases.get(&release)?.media
    }

    /// The pressing a copy's slot sits on. Asked of a slot for the reason
    /// [`Librarian::files_under`] is: a recording is on every release that
    /// holds it, and only the slot says which one this copy came off.
    pub fn release_of(&self, slot: Option<Id<Track>>) -> Option<Id<crate::model::entity::Release>> {
        let track = self.spine.tracks.get(&slot?)?;
        Some(self.spine.media.get(&track.medium.id())?.release.id())
    }

    /// What the authority called the disc a slot sits on — #169.
    ///
    /// `CD`, `12" Vinyl`, `Digital Media`. `None` where nobody said, which is
    /// every record whose tracklist has not been catalogued.
    pub fn medium_format(&self, slot: Option<Id<Track>>) -> Option<String> {
        let track = self.spine.tracks.get(&slot?)?;
        self.spine.media.get(&track.medium.id())?.format.clone()
    }

    /// How a subject is credited, as printed — #152.
    ///
    /// The public door onto `Lookup::line`, so a work list can publish an
    /// album's credit alongside a recording's. A pass needs both to tell a
    /// single from a compilation: they carry the same string on a remix single
    /// and different ones on a Lomax field recording, and only the first may
    /// have its album credit rewritten.
    pub fn credited_line(&self, subject: CreditOn) -> String {
        self.reference.credited_as(subject)
    }

    /// **Positions on a disc that more than one recording claims** — #168.
    ///
    /// A slot is a place on a medium and exactly one thing can be in it, but
    /// nothing enforced that. `Id::<Track>::of_track` mints a slot from
    /// **medium and recording**, which is deliberate — keyed by position,
    /// correcting a tracklist re-mints every row that shifted and strands
    /// whatever hung off the old ones. Its doc names the cost it accepted: the
    /// album that plays one recording twice, which folds to one slot.
    ///
    /// The other direction was not anticipated. Two recordings claiming one
    /// position mint two slots and the library holds both without complaint,
    /// which is how cataloguing 487 MusicBrainz tracklists silently produced
    /// 183 contested positions across 73 media here.
    ///
    /// Returned rather than judged. A disc really can play one recording twice
    /// and a compilation really can carry two versions of a song, so which
    /// claimant is right is a question about evidence and not about counting.
    pub fn contested_slots(
        &self,
    ) -> BTreeMap<(Id<crate::model::entity::Medium>, u16), Vec<Id<Recording>>> {
        let mut at: BTreeMap<(Id<crate::model::entity::Medium>, u16), Vec<Id<Recording>>> =
            BTreeMap::new();
        for slot in self.spine.tracks.values() {
            at.entry((slot.medium.id(), slot.position))
                .or_default()
                .push(slot.recording.id());
        }
        at.retain(|_, claims| {
            claims.sort_unstable();
            claims.dedup();
            claims.len() > 1
        });
        at
    }

    /// The album a recording sits on, if any.
    /// Asked of a slot, for the reason [`Librarian::files_under`] is: a
    /// recording is on every release that holds it.
    pub fn group_of(&self, slot: Option<Id<Track>>) -> Option<Id<ReleaseGroup>> {
        let track = self.spine.tracks.get(&slot?)?;
        let medium = self.spine.media.get(&track.medium.id())?;
        let release = self.spine.releases.get(&medium.release.id())?;
        Some(release.group.id())
    }

    /// Where a recording sits: disc, then track, as the tags claim.
    ///
    /// Sorts unplaced recordings last rather than first, because a track with
    /// no number is a gap in what is known, and putting it at the top of an
    /// album would state something the tags never did.
    pub fn place_of(&self, slot: Option<Id<Track>>) -> (u16, u16) {
        slot.and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| {
                self.spine
                    .media
                    .get(&t.medium.id())
                    .map(|m| (m.position, t.position))
            })
            .map(|(d, tr)| (d, if tr == 0 { u16::MAX } else { tr }))
            .unwrap_or((u16::MAX, u16::MAX))
    }

    /// Everything playable, in a stable order.
    pub fn recordings(&self) -> impl Iterator<Item = Id<Recording>> + '_ {
        self.spine.recordings.keys().copied()
    }

    // **`slot_of` lived here and is gone** (#155).
    //
    // It answered "which slot is this copy in", and its last caller was the
    // tree's sort key — where the question is which slot is this *row* in.
    // The two agree only while every row has a copy, which is the assumption
    // #155 removes: asked of a greyed row it answered `None`, and the row
    // sorted to the bottom of its album instead of into its gap.
    //
    // `placed_at` is the same lookup and is kept, because its callers really
    // are asking about a file — which pressing this copy came off is a fact
    // about the copy.

    // **`listings` lived here and is gone** (#121).
    //
    // It answered "every copy of this recording, as rows would list them" and
    // had one caller: the tree, which asked it per recording and allocated a
    // `Vec` for each — the whole library's worth, before any of them were
    // tested for membership. That was a ~100ms floor no amount of narrowing
    // could get under. The tree walks the members now, and a member *is* a
    // copy, so the listing is built rather than looked up.
    //
    // What went with it is the entry for a recording nothing holds — a
    // wishlist row, which §1 is explicit is ordinary. Nothing lost it here:
    // `Shelf::grouped` already dropped any listing with no copy, so the tree
    // has never drawn one. When the shelf learns to, it needs a set that
    // names recordings rather than copies, not this back.
}

/// **Reading a collection back** — #92.
///
/// Its own block because it was all ahead of its callers when written. `#[allow]`
/// rather than `#[expect]` because tests exercise them, so an expectation would
/// report itself unfulfilled under `cargo test` — that choice still stands, and #113
/// is where the general preference for `expect` came from.
///
/// **The premise it used to name has expired** (#250). It said "a playlist is still a
/// file on disk until #92's UI half"; a playlist is in the log now — `load_collection`
/// resolves `Id::<Collection>::of_name` and reads no path — and #92 is closed. Three
/// sibling attributes in `instance.rs` carried the same sentence over functions with
/// six and eight call sites, and went when it did.
#[allow(
    dead_code,
    reason = "read back by tests and by the collection surface; kept as one block"
)]
impl Librarian {
    /// What a collection holds, in order.
    ///
    /// **A sequence, so duplicates come back as duplicates** (#92). The same
    /// recording twice in one playlist is two entries at two positions, and a
    /// `Vec` is the only shape that can say so — a set could not, which is
    /// exactly why a view is not only a predicate.
    ///
    /// Already in order: the fold keys entries by position before recording,
    /// so walking them is walking the list.
    pub fn entries_of(&self, collection: Id<Collection>) -> Vec<Id<Recording>> {
        <Self as crate::model::rows::Listing>::entries_of(self, collection)
    }

    fn collection_entries(&self, collection: Id<Collection>) -> Vec<Id<Recording>> {
        self.collected
            .iter()
            .filter(|(c, _, _)| *c == collection)
            .map(|(_, _, recording)| *recording)
            .collect()
    }

    /// The position of each entry, for a caller inserting between two of them.
    ///
    /// Separate from [`Librarian::entries_of`] because they are asked at
    /// different moments: the recordings are what a list shows, and the keys
    /// are what an edit needs — and an edit needs the two it is landing
    /// between rather than all of them.
    pub fn positions_of(&self, collection: Id<Collection>) -> Vec<OrdKey> {
        self.collected
            .iter()
            .filter(|(c, _, _)| *c == collection)
            .map(|(_, ord, _)| ord.clone())
            .collect()
    }

    /// Every collection there is, by id.
    pub fn collections(&self) -> &BTreeMap<Id<Collection>, Collected> {
        &self.collections
    }
}

/// **The librarian answers the row contract** — #86, and the point of it.
///
/// `Scan` implemented this and the player read rows through the store. Now the
/// library draws them, which is what lets the store go: the trait is the seam,
/// and swapping which side of it answers is the whole migration.
impl crate::model::rows::Listing for Librarian {
    fn entries_of(&self, collection: Id<Collection>) -> Vec<Id<Recording>> {
        self.collection_entries(collection)
    }

    fn find(&self, of: crate::model::about::Kind, name: &str) -> crate::model::about::Found {
        crate::model::about::find(self, of, name)
    }

    fn covers(&self, focus: crate::model::focus::Focus) -> Vec<Id<Recording>> {
        Librarian::covers(self, focus)
    }

    fn label_of(&self, focus: crate::model::focus::Focus) -> String {
        crate::model::about::label_of(self, focus)
    }

    fn about(
        &self,
        focus: crate::model::focus::Focus,
        happened: &[Happened],
    ) -> crate::model::about::About {
        crate::model::about::of(self, focus, happened)
    }

    /// Whether any narrower-of edge places a concept on this axis.
    ///
    /// Asked of the child, because an edge is only useful for climbing if the
    /// thing being climbed from is on the axis in question — a version
    /// hierarchy must not make `levels genre@1` claim it has somewhere to go
    /// (#117 keeps the axes apart, and this is where that matters).
    fn places(&self, facet: Id<crate::model::entity::Facet>) -> bool {
        // **The id says which axis it is on**, because it was minted from the
        // facet key and the label (#117). So re-deriving from the term's name
        // and comparing is the membership test, and no second map is needed to
        // answer it.
        let on_axis = |id: Id<crate::model::entity::Concept>| {
            self.vocabulary.genres.get(&id).is_some_and(|term| {
                crate::model::facet::ALL.iter().any(|(key, _)| {
                    Id::<crate::model::entity::Facet>::of_key(key) == facet
                        && Id::<crate::model::entity::Concept>::in_facet(key, &term.name) == id
                })
            })
        };
        self.vocabulary.edges.iter().any(|e| on_axis(e.child.id()))
    }

    fn whole(&self) -> Option<&Librarian> {
        Some(self)
    }

    fn row(&self, recording: Id<Recording>) -> Option<Row> {
        Librarian::row(self, recording)
    }

    fn chosen(
        &self,
        recording: Id<Recording>,
        here: &[Id<Device>],
        prefers: &[crate::model::encoding::Format],
    ) -> Option<(String, Id<Encoding>, crate::model::encoding::Levels)> {
        Librarian::open(self, recording, here, prefers)
            .map(|(e, locator)| (locator.to_owned(), e.id, e.levels))
    }

    /// **Where a subject comes from now** — `Listing::subject` is gone (#261).
    ///
    /// It handed back a `Subject<'_>`, which holds a `Listed` borrowed from the fold, and this
    /// seam cannot promise a borrow into something a remote reader does not have. The filter
    /// builds its own from a concrete library, which is the hot path; a caller holding only the
    /// seam builds one from a [`crate::model::rows::Slip`] it owns.
    fn listed(&self, id: crate::model::rows::CopyId) -> Option<crate::model::rows::Slip> {
        self.describe(id).as_ref().map(crate::model::rows::Slip::of)
    }

    fn rows(&self) -> Vec<crate::model::rows::CopyId> {
        self.rows.keys().copied().collect()
    }

    /// **Which performance this row is of.** It used to be the binding held
    /// for a row's *file*, because a row was a file. A row is an appearance
    /// now (#155) and the slot names its recording; a loose row is one.
    fn recording_of(&self, id: crate::model::rows::CopyId) -> Option<Id<Recording>> {
        use crate::model::rows::CopyId;
        match id {
            CopyId::On(track) => Some(self.spine.tracks.get(&track)?.recording.id()),
            CopyId::Loose(recording) => self
                .spine
                .recordings
                .contains_key(&recording)
                .then_some(recording),
        }
    }

    fn playable(&self, id: crate::model::rows::CopyId) -> Option<crate::model::rows::Playable> {
        // **Which copy would open.** A row is an appearance and an appearance
        // has no locator; playing one is a question about a file, asked here
        // rather than at listing time.
        let copy = self.shown_copy(id)?;
        let levels = &self.encodings.encodings.get(&copy)?.levels;
        Some(crate::model::rows::Playable {
            // Only a holding on *this* device. A row the library knows about
            // and this machine does not hold is an ordinary row and an
            // unplayable one, which is a different answer from "no such row".
            locator: self.locators.get(&copy)?.clone(),
            gain: (!levels.is_empty()).then(|| crate::model::rows::Loudness {
                track_db: levels.track_db(),
                track_peak: levels.track_peak(),
                album_db: levels.album_db(),
                album_peak: levels.album_peak(),
            }),
        })
    }
}

/// The claims about one subject on one axis, before they are collapsed to
/// labels: how confident, how deep in the hierarchy, and what it is called.
type Ranked<'a> = Vec<(Confidence, usize, &'a str)>;

/// A recording, and which copy of it a row is about.
///
/// The pair `row_of` takes, named so a browse tree can carry it. `copy` is
/// `None` for a recording nothing here holds — an ordinary row, not a missing
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listing {
    pub recording: Id<Recording>,
    /// **What this row is** — its own identity (#155).
    pub row: crate::model::rows::CopyId,
    /// The copy filling it, where anything does.
    pub copy: Option<Id<Encoding>>,
}

/// One displayed row.
///
/// Holds identities and values, **never a location**. That omission is the
/// point: a `PathBuf` on a row is what let `history.toml` be keyed by path,
/// which cost three stranded entries and a four-star rating on a file that
/// still existed.
///
/// Every field here is something `src/ui/` reads off a `Track` today. Two of
/// them dissolve rather than move:
///
/// - **`title` never needs a file name.** `Track::display_title` falls back to
///   the path's stem, but ingest already did that (#70), so by the time a
///   recording exists its title is a title.
/// - **`file_under` stops being a heuristic.** "Album artist, else artist" is
///   guessing at a fact the model states: ask the release group's credit, then
///   the recording's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub recording: Id<Recording>,
    pub title: String,
    /// Who this files under — the album-level credit where there is one.
    pub file_under: String,
    pub album: Option<String>,
    pub length: std::time::Duration,
    pub rating: Option<u8>,
    /// Counted from the log, never a stored number.
    pub plays: u32,
    pub format: Option<Format>,
    pub bitrate: Option<u32>,
    /// What the canonical encoding measures as, so a re-buy list is a filter
    /// rather than a separate pass.
    pub verdict: Option<Verdict>,
    pub available: bool,
}

/// **How much of a release this library holds** — #134.
///
/// Three answers, and the third is the point. "I do not know how many there
/// are" must not read as "I have them all": 12% of this library carries no
/// total, and a release nothing identified never will.
///
/// **Serialisable because a `Slip` carries one** — #261. A row that crossed the wire has to say
/// which of the three answers it is, and "I do not know" is the one that must survive the trip.
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
pub enum Held {
    /// Every disc it is known to have, and every track on each of them.
    Complete,
    /// Some of it, and the library knows there is more.
    Partial,
    /// Nobody said how many there are.
    #[default]
    Unknown,
}

impl Held {
    pub const fn written(self) -> &'static str {
        match self {
            Held::Complete => "complete",
            Held::Partial => "partial",
            Held::Unknown => "unknown",
        }
    }
}

/// **How much of every pressing is here** — #134, worked out in one pass.
///
/// Two counts have to agree for "complete", and the second is the one a naive
/// version forgets: a release whose second disc has never been seen has **no
/// medium row at all**, so counting held tracks against the discs present
/// answers "all of them" for half a record. The pressing's own disc count is
/// what closes that.
///
/// Either count missing is [`Held::Unknown`] — for the whole release, not just
/// the disc that was quiet, because a release is complete only if all of it is
/// and an unknown part makes the whole unknown.
///
/// **One pass, not one per row.** Asking this per copy was a scan of the spine
/// per copy, and it cost 97 ms of a 124 ms fold on a 6,500-row library.
fn held_index(
    spine: &crate::model::spine::Spine,
    // **Which slots a file actually fills** — #155, and the count would be a
    // lie without it. This counted *slots*, which was right only while nothing
    // wrote a slot it had no copy for: cataloguing a whole tracklist makes
    // every disc look full the moment it is known, so an album you are three
    // tracks short of would answer `Complete`. The question is how much of the
    // record is **here**, and a slot is not a file.
    placed: &std::collections::BTreeMap<Id<Encoding>, Id<Track>>,
) -> std::collections::BTreeMap<Id<crate::model::entity::Release>, Counted> {
    let filled: std::collections::BTreeSet<Id<Track>> = placed.values().copied().collect();
    let mut on_medium: std::collections::BTreeMap<Id<crate::model::entity::Medium>, usize> =
        Default::default();
    for track in spine.tracks.values().filter(|t| filled.contains(&t.id)) {
        *on_medium.entry(track.medium.id()).or_default() += 1;
    }

    // Per release: how many discs are here, and whether every one of them is
    // full. `short` is set by a disc that is missing tracks, `quiet` by a disc
    // that never said how many it has.
    struct Tally {
        discs: usize,
        here: usize,
        /// The sum across discs, and `None` the moment one disc is quiet — a
        /// total missing a disc is not a total.
        total: Option<u16>,
        short: bool,
        quiet: bool,
    }
    let mut tally: std::collections::BTreeMap<Id<crate::model::entity::Release>, Tally> =
        Default::default();
    for medium in spine.media.values() {
        let t = tally.entry(medium.release.id()).or_insert(Tally {
            discs: 0,
            here: 0,
            total: Some(0),
            short: false,
            quiet: false,
        });
        t.discs += 1;
        let here = on_medium.get(&medium.id).copied().unwrap_or(0);
        t.here += here;
        match medium.tracks {
            None => {
                t.quiet = true;
                t.total = None;
            }
            Some(total) => {
                t.short |= here < usize::from(total);
                t.total = t.total.map(|sum| sum.saturating_add(total));
            }
        }
    }

    spine
        .releases
        .values()
        .map(|release| {
            let tallied = tally.get(&release.id);
            let answer = match (release.media, tallied) {
                // No disc count, so an unseen disc cannot be ruled out.
                (None, _) | (_, None) => Held::Unknown,
                // A disc that is not here at all. Known short before anything
                // else is asked, which is why this comes before `quiet`.
                (Some(discs), Some(t)) if usize::from(discs) > t.discs => Held::Partial,
                (Some(_), Some(t)) if t.short => Held::Partial,
                (Some(_), Some(t)) if t.quiet => Held::Unknown,
                _ => Held::Complete,
            };
            (
                release.id,
                Counted {
                    answer,
                    here: tallied.map(|t| t.here).unwrap_or(0),
                    // **No total when a disc is missing entirely**, because
                    // the discs that are here do not add up to the record.
                    total: tallied.and_then(|t| t.total).filter(|_| {
                        release
                            .media
                            .is_some_and(|d| usize::from(d) <= tallied.map_or(0, |t| t.discs))
                    }),
                },
            )
        })
        .collect()
}

/// How much of one pressing is here, and out of how many — #134, #137.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counted {
    pub answer: Held,
    /// Tracks of this pressing the library holds.
    pub here: usize,
    /// Tracks the pressing has, when every disc said and every disc is here.
    pub total: Option<u16>,
}

/// What is shown when nothing names the artist.
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::facet;
    use std::path::Path;

    fn read(dir: &str) -> Vec<(String, String)> {
        // **A missing directory is a failure, not an empty answer** — #213.
        //
        // The walk below skips an unreadable directory and carries on, which
        // is right for a subdirectory that vanished mid-walk and was very
        // wrong for the argument: every boundary ratchet built on this counts
        // occurrences and asserts the count has not risen, so a path that no
        // longer exists turns the whole test into `assert!(0 <= 0)`.
        //
        // The crate split moved `src/model` under `crates/gmuse-library/` and
        // took `src/ui` out of this crate entirely. The model tests kept
        // working — cargo runs them with the package root as the working
        // directory — and the one pointed at `src/ui` silently stopped
        // checking anything. It passed. That is the same lesson as the
        // degenerate fixture in §13, arriving through a different door.
        assert!(
            Path::new(dir).is_dir(),
            "{dir} is not a directory from {:?} — a boundary test that reads \
             nothing passes for the wrong reason",
            std::env::current_dir().unwrap_or_default()
        );
        let mut out = Vec::new();
        let mut stack = vec![Path::new(dir).to_path_buf()];
        while let Some(p) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&p) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs")
                    && let Ok(text) = std::fs::read_to_string(&path)
                {
                    out.push((path.display().to_string(), text));
                }
            }
        }
        out
    }

    /// **The direction that holds, asserted so it keeps holding.**
    ///
    /// The librarian must not know there is a player. Nothing here reaches for
    /// a terminal, an audio device, or a key binding — and the day something
    /// does, this fails rather than the seam quietly closing up.    /// **A partial album needs a denominator, and the disc count is half of
    /// it** — #134.
    ///
    /// Counting held tracks against the discs *present* answers "all of them"
    /// for a two-disc set whose second disc has never been seen: there is no
    /// medium row for it, so there is nothing to be short of. Only the
    /// pressing's own disc count closes that.
    #[test]
    fn how_much_of_a_release_is_here() {
        use crate::model::entity::{Medium, Release};
        use crate::model::spine;

        // One pressing, said to be two discs of two tracks each.
        let release = Id::<Release>::derived(b"pressing");
        let build = |discs: Option<u16>, media: &[(u16, Option<u16>, usize)]| {
            let mut l = Librarian::default();
            l.spine.releases.insert(
                release,
                spine::Release {
                    id: release,
                    group: crate::model::id::Ref::to(Id::derived(b"group")),
                    title: "Two Discs".into(),
                    released: None,
                    label: None,
                    catalogue_number: None,
                    media: discs,
                },
            );
            for (position, tracks, here) in media {
                let m = Id::<Medium>::of_medium(release, *position);
                l.spine.media.insert(
                    m,
                    spine::Medium {
                        id: m,
                        release: crate::model::id::Ref::to(release),
                        position: *position,
                        format: None,
                        tracks: *tracks,
                    },
                );
                for n in 0..*here {
                    let id = Id::<crate::model::entity::Track>::derived(
                        format!("{position}-{n}").as_bytes(),
                    );
                    l.spine.tracks.insert(
                        id,
                        spine::Track {
                            id,
                            medium: crate::model::id::Ref::to(m),
                            recording: crate::model::id::Ref::to(Id::derived(
                                format!("r{position}-{n}").as_bytes(),
                            )),
                            position: n as u16 + 1,
                            title_on_release: None,
                        },
                    );
                    // **A slot and a copy of it**, which is what `here` has
                    // always meant. It used to be enough to write the slot,
                    // because nothing wrote one it had no file for; #155 does,
                    // so the file has to be said out loud.
                    l.placed.insert(
                        Id::<Encoding>::derived(format!("e{position}-{n}").as_bytes()),
                        id,
                    );
                }
            }
            // The index, which is where the logic lives now (#137) — the
            // librarian's `held_of` is a lookup into exactly this.
            l.held = held_index(&l.spine, &l.placed);
            l.held_of(release)
        };

        assert_eq!(
            build(Some(2), &[(1, Some(2), 2), (2, Some(2), 2)]),
            Held::Complete,
            "both discs, both full"
        );
        assert_eq!(
            build(Some(2), &[(1, Some(2), 1), (2, Some(2), 2)]),
            Held::Partial,
            "a track short on the first disc"
        );

        // **The trap.** Disc two was never seen, so it has no medium row and
        // nothing to be short of. Counting only what is present says complete.
        assert_eq!(
            build(Some(2), &[(1, Some(2), 2)]),
            Held::Partial,
            "a whole disc missing read as complete — the reason `media` exists"
        );

        // **Unknown is a third answer, not a hedge.** 12% of this library
        // carries no total, and reading that as "I have them all" is the
        // wrong answer this field exists to stop.
        assert_eq!(
            build(None, &[(1, Some(2), 2)]),
            Held::Unknown,
            "no disc count, so an unseen disc cannot be ruled out"
        );
        assert_eq!(
            build(Some(1), &[(1, None, 2)]),
            Held::Unknown,
            "no track count on the disc that is here"
        );
        assert_eq!(
            build(Some(2), &[(1, Some(2), 2), (2, None, 2)]),
            Held::Unknown,
            "one disc quiet makes the release unknown, not complete"
        );

        // A release the library does not hold at all.
        assert_eq!(
            Librarian::default().held_of(release),
            Held::Unknown,
            "nothing known about a release nothing holds"
        );
    }

    /// **Knowing what a record is must not be mistaken for owning it** — #155.
    ///
    /// The regression the second half of that issue walks straight into.
    /// `held_index` counted slots, which was right only while a slot and a
    /// copy were written together; cataloguing a tracklist writes slots for
    /// tracks you do not have, and counting those says the album is complete
    /// the moment you learn how long it is. The number wanted is how much is
    /// **here**.
    #[test]
    fn a_catalogued_track_you_do_not_hold_does_not_count_as_held() {
        use crate::model::entity::{Medium, Release};
        use crate::model::spine;

        let release = Id::<Release>::derived(b"pressing");
        let medium = Id::<Medium>::of_medium(release, 1);
        let mut l = Librarian::default();
        l.spine.releases.insert(
            release,
            spine::Release {
                id: release,
                group: crate::model::id::Ref::to(Id::derived(b"group")),
                title: "Three Tracks".into(),
                released: None,
                label: None,
                catalogue_number: None,
                media: Some(1),
            },
        );
        l.spine.media.insert(
            medium,
            spine::Medium {
                id: medium,
                release: crate::model::id::Ref::to(release),
                position: 1,
                format: None,
                tracks: Some(3),
            },
        );
        // Three slots, as a tracklist would write them, and a file for two.
        for n in 0..3u16 {
            let track = Id::<crate::model::entity::Track>::derived(&n.to_le_bytes());
            l.spine.tracks.insert(
                track,
                spine::Track {
                    id: track,
                    medium: crate::model::id::Ref::to(medium),
                    recording: crate::model::id::Ref::to(Id::derived(b"r")),
                    position: n + 1,
                    title_on_release: None,
                },
            );
            if n < 2 {
                l.placed
                    .insert(Id::<Encoding>::derived(&n.to_le_bytes()), track);
            }
        }
        l.held = held_index(&l.spine, &l.placed);

        assert_eq!(
            l.held_of(release),
            Held::Partial,
            "three slots and two files is two thirds of a record, not all of it"
        );
        assert_eq!(
            l.counted_of(release),
            Some((2, 3)),
            "the numerator is files and the denominator is slots"
        );
    }

    /// **An empty slot at a position something already fills is not a gap** —
    /// #168.
    ///
    /// The gap rule is #134's: a slot with no copy still draws when the
    /// library holds *something else* off that medium, so a record you own
    /// eleven of fifteen tracks of shows four holes rather than pretending to
    /// be complete. What that cannot tell on its own is a hole from a shadow.
    ///
    /// `of_track` seeds a slot from medium and recording, so two passes
    /// disagreeing about which recording sits at a position mint two ids for
    /// it. A relink re-points the one it finds, both come to name the same
    /// performance, and both ids remain — the same place on the same disc,
    /// twice, one playable and one not. *21* drew ten of its fifteen tracks
    /// that way, which is what made this visible at all.
    #[test]
    fn a_slot_shadowing_a_filled_position_is_not_drawn_as_a_hole() {
        use crate::model::entity::{Encoding, Medium, Release, Track};
        use crate::model::rows::{CopyId, Listing};
        use crate::model::spine;

        let release = Id::<Release>::derived(b"21");
        let medium = Id::<Medium>::of_medium(release, 1);
        let mut l = Librarian::default();

        let place = |l: &mut Librarian, recording: Id<Recording>, position: u16| -> Id<Track> {
            let slot = Id::<Track>::of_track(medium, recording);
            l.spine.tracks.insert(
                slot,
                spine::Track {
                    id: slot,
                    medium: crate::model::id::Ref::to(medium),
                    recording: crate::model::id::Ref::to(recording),
                    position,
                    title_on_release: None,
                },
            );
            slot
        };

        // Track 1 is held. Track 2 is a genuine hole. And track 1 also carries
        // a second slot, minted when a pass disagreed about what sits there.
        let one = Id::<Recording>::derived(b"track one");
        let shadow = Id::<Recording>::derived(b"what track one used to be");
        let missing = Id::<Recording>::derived(b"track two, which we lack");
        let held = place(&mut l, one, 1);
        place(&mut l, shadow, 1);
        place(&mut l, missing, 2);

        let copy = Id::<Encoding>::derived(b"the one file");
        l.placed.insert(copy, held);
        l.encodings.encodings.insert(
            copy,
            crate::model::encoding::Encoding {
                id: copy,
                audio: Default::default(),
                recording: Some(crate::model::id::Ref::to(one)),
                format: crate::model::encoding::Format::Flac,
                bitrate: 900,
                sample_rate: None,
                channels: None,
                length: std::time::Duration::from_secs(200),
                processed: false,
                cutoff_hz: None,
                cliff_db: None,
                gain: None,
                levels: Default::default(),
                encoder_delay: None,
            },
        );
        for id in [one, shadow, missing] {
            l.spine.recordings.insert(
                id,
                crate::model::spine::Recording {
                    id,
                    title: "Whatever".into(),
                    work: None,
                    recorded: None,
                    bpm: None,
                    length: Default::default(),
                    pinned: None,
                },
            );
        }
        l.held = held_index(&l.spine, &l.placed);
        l.draw();

        let drawn: Vec<_> = Listing::rows(&l);
        assert!(
            drawn.contains(&CopyId::On(held)),
            "the slot with the file is the row"
        );
        assert!(
            drawn.contains(&CopyId::On(Id::<Track>::of_track(medium, missing))),
            "a position nothing fills is a hole in the record, and #134 wants it shown"
        );
        assert!(
            !drawn.contains(&CopyId::On(Id::<Track>::of_track(medium, shadow))),
            "track 1 is not missing — it is right there — so a second slot on it \
             is a shadow and not a hole"
        );
    }

    /// **A loose row is a row because a file makes it one** — #168.
    ///
    /// A recording on no release is drawn, because a standalone recording is
    /// real and this library has one. But nothing asked whether anything held
    /// it, and the slot branch beside it always did: a slot is a row when a
    /// copy fills it or when the library holds something else off that medium,
    /// so a part-held album shows its gaps (#134) and an album nobody owns
    /// shows nothing.
    ///
    /// What the missing question let through is residue. An identification
    /// that moves leaves the recording it moved *from* holding no file and
    /// sitting in no slot, and 100 of those had accumulated here — drawn
    /// beside the real rows, playable by nothing, and indistinguishable in the
    /// list from a track genuinely missing off a record. They are not
    /// standalone recordings; they are the shadow of a claim withdrawn.
    #[test]
    fn a_recording_nothing_holds_and_no_disc_places_is_not_a_row() {
        use crate::model::entity::Encoding;
        use crate::model::rows::{CopyId, Listing};
        use crate::model::spine::Recording as Rec;

        let standalone = Id::<Recording>::derived(b"a real standalone recording");
        let residue = Id::<Recording>::derived(b"what a relink moved away from");

        let mut l = Librarian::default();
        for (id, title) in [(standalone, "A One-Off"), (residue, "What It Used To Be")] {
            l.spine.recordings.insert(
                id,
                Rec {
                    id,
                    title: title.into(),
                    work: None,
                    recorded: None,
                    bpm: None,
                    length: Default::default(),
                    pinned: None,
                },
            );
        }
        // Only the first has bytes behind it.
        let copy = Id::<Encoding>::derived(b"a file");
        l.encodings.encodings.insert(
            copy,
            crate::model::encoding::Encoding {
                id: copy,
                audio: Default::default(),
                recording: Some(crate::model::id::Ref::to(standalone)),
                format: crate::model::encoding::Format::Flac,
                bitrate: 320,
                sample_rate: None,
                channels: None,
                length: std::time::Duration::from_secs(180),
                processed: false,
                cutoff_hz: None,
                cliff_db: None,
                gain: None,
                levels: Default::default(),
                encoder_delay: None,
            },
        );
        l.draw();

        let drawn: Vec<_> = Listing::rows(&l);
        assert!(
            drawn.contains(&CopyId::Loose(standalone)),
            "a standalone recording with a file is exactly what a loose row is for"
        );
        assert!(
            !drawn.contains(&CopyId::Loose(residue)),
            "a recording with no copy and no slot is a row on no evidence at all"
        );
    }

    /// **Every fact that undoes another has a way to be written.**
    ///
    /// Three facts exist to take something back — a copy retired, a concept
    /// asserted, a narrowing — and each one's own doc says why it must: *"a
    /// step you cannot take back is one nobody takes"*. That argument is about
    /// the **operator**, not the fold, so a fact nothing can emit satisfies it
    /// on paper and not at all.
    ///
    /// Found twice by walking into it. `EncodingRestored` was defined,
    /// documented and folded, and no command or proposal reached it — so
    /// retiring 53 files was reversible in the model and a manual file move
    /// plus a hand-written log entry in practice. `ConceptRetracted` is the
    /// same shape and is still open.
    ///
    /// The check is deliberately shallow: somewhere outside `log.rs` names
    /// this fact. It cannot tell whether the route is *reachable*, only that
    /// one was written, which is exactly the difference between the two states
    /// this has caught.
    #[test]
    fn a_fact_that_undoes_another_can_be_written() {
        let elsewhere: Vec<String> = read("src")
            .into_iter()
            .filter(|(path, _)| !path.contains("log.rs"))
            .map(|(_, text)| text)
            .collect();
        let emitted = |fact: &str| elsewhere.iter().any(|t| t.contains(fact));

        assert!(
            emitted("EncodingRestored"),
            "`storage-layout.md` §6 makes retiring reversible, so something has \
             to be able to say a copy is back"
        );
        assert!(
            emitted("EncodingRetired"),
            "and something has to be able to retire one"
        );
        assert!(
            emitted("NarrowingRetracted"),
            "a narrowing that cannot be withdrawn is a vocabulary nobody can correct"
        );
        assert!(
            emitted("CreditRetracted"),
            "#145's withdrawal is the only thing that can undo a joined credit"
        );
    }

    /// **A contested position is what a fingerprint is for.**
    ///
    /// Both claimants are called the same thing, both sit on the same record,
    /// and the only difference between them is which audio is in the seat —
    /// so a name settles nothing and a length often cannot either. Measured
    /// over the first 80 such copies here, duration left 36 undecidable and a
    /// fingerprint identified 36, of which 16 moved the file to a different
    /// recording.
    ///
    /// This is the half the model owes the pass: the recordings a tool has to
    /// listen to, which is every claimant something here can actually open.
    #[test]
    fn a_contested_position_names_the_copies_to_listen_to() {
        use crate::model::entity::{Medium, Release, Track};
        use crate::model::spine;

        let release = Id::<Release>::derived(b"a pressing");
        let medium = Id::<Medium>::of_medium(release, 1);
        let one = Id::<Recording>::derived(b"what the file says");
        let other = Id::<Recording>::derived(b"what the tracklist says");
        let elsewhere = Id::<Recording>::derived(b"an uncontested seat");

        let mut l = Librarian::default();
        let mut seat = |recording: Id<Recording>, position: u16| {
            let slot = Id::<Track>::of_track(medium, recording);
            l.spine.tracks.insert(
                slot,
                spine::Track {
                    id: slot,
                    medium: crate::model::id::Ref::to(medium),
                    recording: crate::model::id::Ref::to(recording),
                    position,
                    title_on_release: None,
                },
            );
        };
        seat(one, 4);
        seat(other, 4);
        seat(elsewhere, 5);

        let named: std::collections::BTreeSet<Id<Recording>> =
            l.contested_slots().into_values().flatten().collect();
        assert!(
            named.contains(&one) && named.contains(&other),
            "both claimants have to be listened to — the point is telling them apart"
        );
        assert!(
            !named.contains(&elsewhere),
            "a seat nobody disputes is not a question, and fingerprinting it \
             spends a rate limit to be told what the library already knows"
        );
    }

    /// **A position on a disc holds one thing, and nothing enforced that** —
    /// #168.
    ///
    /// `of_track` mints a slot from medium *and recording*, deliberately: keyed
    /// by position, correcting a tracklist re-mints every row that shifted. The
    /// cost its doc names is one recording in two positions, which folds. The
    /// cost it missed is two recordings in one position, which does not — they
    /// are two slots, and the library held 183 such pairs without complaint.
    ///
    /// The real one: `Adele/21/11 Someone Like You.mp3` runs 295.4s and was
    /// identified as MusicBrainz's live Royal Albert Hall recording (294.0s)
    /// over the studio one (285.0s), on 1.4 seconds against 10. Cataloguing
    /// *21*'s tracklist then put the studio recording in the same position,
    /// and the row nobody can play is the one that is right.
    #[test]
    fn two_recordings_claiming_one_position_are_reported() {
        use crate::model::entity::{Medium, Release, Track};
        use crate::model::spine;

        let release = Id::<Release>::derived(b"21");
        let medium = Id::<Medium>::of_medium(release, 1);
        let live = Id::<Recording>::derived(b"royal albert hall");
        let studio = Id::<Recording>::derived(b"the studio take");

        fn place(l: &mut Librarian, medium: Id<Medium>, recording: Id<Recording>, position: u16) {
            let slot = Id::<Track>::of_track(medium, recording);
            l.spine.tracks.insert(
                slot,
                spine::Track {
                    id: slot,
                    medium: crate::model::id::Ref::to(medium),
                    recording: crate::model::id::Ref::to(recording),
                    position,
                    title_on_release: None,
                },
            );
        }

        let mut l = Librarian::default();
        place(&mut l, medium, live, 11);
        assert!(
            l.contested_slots().is_empty(),
            "one recording in a position is the ordinary case"
        );

        // The tracklist arrives and says something else belongs there. Because
        // the id carries the recording, this is a second slot rather than a
        // correction of the first — which is the whole defect.
        place(&mut l, medium, studio, 11);
        let contested = l.contested_slots();
        assert_eq!(
            contested.get(&(medium, 11)).map(Vec::len),
            Some(2),
            "two recordings claim track 11 and nothing said so"
        );

        // One recording in two positions is the case `of_track` documented as
        // acceptable: it folds to a single slot, so there is nothing to report.
        let mut l = Librarian::default();
        place(&mut l, medium, live, 3);
        place(&mut l, medium, live, 9);
        assert!(
            l.contested_slots().is_empty(),
            "a reprise folds to one slot, which `of_track` accepted on purpose"
        );
    }

    /// **The name of the thing in the slot, not the name of the recording** —
    /// #310 wrote it down and nothing read it.
    ///
    /// A release prints what it prints. MusicBrainz's recording is `The Fox`
    /// and *Live 1975-85* prints `The Fox (live)`; the recording is `Phobos`
    /// and *Luna* prints `Phobos (Synthapella)`. Both names are correct and
    /// they name different things, so the row for a copy sitting in that slot
    /// takes the sleeve's.
    ///
    /// The consequence of skipping it is not cosmetic: the storage tree is
    /// built from the row, so cataloguing 487 tracklists would have renamed
    /// 87 files to the plainer name and lost `(live)`, `(acoustic)` and
    /// `(Synthapella)` off disc.
    #[test]
    fn a_row_takes_the_name_the_sleeve_prints() {
        use crate::model::entity::{Medium, Release, Track};
        use crate::model::spine;
        use crate::model::spine::Recording as Rec;

        let rec = Id::<Recording>::derived(b"the fox");
        let release = Id::<Release>::derived(b"live 1975-85");
        let medium = Id::<Medium>::of_medium(release, 1);
        let slot = Id::<Track>::derived(b"slot");

        let mut l = Librarian::default();
        l.spine.recordings.insert(
            rec,
            Rec {
                id: rec,
                title: "The Fox".into(),
                work: None,
                recorded: None,
                bpm: None,
                length: Default::default(),
                pinned: None,
            },
        );
        l.spine.media.insert(
            medium,
            spine::Medium {
                id: medium,
                release: crate::model::id::Ref::to(release),
                position: 1,
                format: None,
                tracks: Some(1),
            },
        );
        l.spine.tracks.insert(
            slot,
            spine::Track {
                id: slot,
                medium: crate::model::id::Ref::to(medium),
                recording: crate::model::id::Ref::to(rec),
                position: 1,
                title_on_release: Some("The Fox (live)".into()),
            },
        );

        assert_eq!(
            l.row(rec).map(|r| r.title),
            Some("The Fox".into()),
            "asked about the recording and not any appearance of it, \
             the recording's own name is the answer"
        );

        let copy = Id::<Encoding>::derived(b"a file");
        l.placed.insert(copy, slot);
        l.held = held_index(&l.spine, &l.placed);
        assert_eq!(
            l.row_of(rec, Some(copy)).map(|r| r.title),
            Some("The Fox (live)".into()),
            "the copy sits in a slot whose sleeve prints a longer name, \
             and that is the name of the thing there"
        );

        // And the other direction. *Frank's Wild Years* holds both
        // `Innocent When You Dream (78)` and `(Barroom)`; the compilation has
        // room for one and prints the bare title, so here the sleeve is the
        // name that lost the qualifier and the recording keeps it.
        l.spine.recordings.get_mut(&rec).unwrap().title = "Innocent When You Dream (78)".into();
        l.spine.tracks.get_mut(&slot).unwrap().title_on_release =
            Some("Innocent When You Dream".into());
        assert_eq!(
            l.row_of(rec, Some(copy)).map(|r| r.title),
            Some("Innocent When You Dream (78)".into()),
            "a compilation that drops the qualifier does not un-distinguish \
             two recordings that need distinguishing"
        );

        // A prefix is not a qualifier. Two different songs, one of whose names
        // begins with the other, stay two songs.
        l.spine.recordings.get_mut(&rec).unwrap().title = "Home Altars of Mexico".into();
        l.spine.tracks.get_mut(&slot).unwrap().title_on_release = Some("Home".into());
        assert_eq!(
            l.row_of(rec, Some(copy)).map(|r| r.title),
            Some("Home".into()),
            "no bracket, so nothing was qualified and the sleeve is describing \
             its own disc"
        );
    }

    #[test]
    fn the_model_does_not_know_the_player_exists() {
        let forbidden = [
            "crate::ui",
            "crate::cmd",
            "crate::audio::",
            "crate::store",
            "crate::library",
            "crate::session",
            "crate::next",
            "crate::playlist",
            "ratatui",
            "crossterm",
            "cpal",
        ];
        // Only `use` lines: "depends on" means importing, and a test that
        // scanned raw text would trip over this very list.
        let mut leaks = Vec::new();
        for (file, text) in read("src/model") {
            for line in text.lines().map(str::trim) {
                if !line.starts_with("use ") {
                    continue;
                }
                for f in forbidden {
                    if line.contains(f) {
                        leaks.push(format!("{file}: {line}"));
                    }
                }
            }
        }
        assert!(
            leaks.is_empty(),
            "the model reached into the player: {leaks:#?}"
        );
    }

    /// **The direction that does not hold yet**, with a number on it.
    ///
    /// `src/ui/` still names a `Scan` and still handles `PathBuf`s. Asserting
    /// zero today would just fail; asserting *no worse than today* turns a
    /// 10,000-line migration into a ratchet, and makes any new leak a test
    /// failure rather than something to be found later.
    ///
    /// **Lower these as the migration lands. Never raise them** — and where
    /// one stops falling, say why rather than leaving it at a number.
    ///
    /// Two of the three have now been revised rather than driven to zero,
    /// because zero was never the right target for them. `PathBuf` counted
    /// four legitimate uses and could not fall further; it is a question now
    /// instead of a count. `Scan` cannot reach zero while the type exists,
    /// which is #86. Only `reaches` still measures a migration in progress,
    /// and its own floor is four.
    ///
    /// **It counts the substring, so prose counted too.** Two `Scan`
    /// mentions were the word *Stored* in doc comments and one was a comment
    /// naming `Scan::recording_of` — three of five, in a number somebody was
    /// going to chase to zero. Reworded rather than subtracted, so the count
    /// means what it says. What is left is the import, the `Context` field,
    /// and one comment about what playlists hold.
    ///
    /// **Test code is not counted**, and that is a correction rather than a
    /// loophole. Counting it measured the wrong thing twice over: most of what
    /// it found was fixtures building a library to test against, and the first
    /// test written to *prove* the seam had moved — that a rename can no
    /// longer strand a rating — failed the ratchet for naming the store it
    /// renamed a file in. A fixture is not a leak. What leaks is the player.
    #[test]
    fn the_leak_from_the_player_only_narrows() {
        /// **Zero** (#86). The player does not name the store at all.
        ///
        /// The last two were the import and the `Context` field, and they went
        /// together because they were the same thing: `Context.store` was what
        /// the import was for. Every read that went through it now goes to the
        /// librarian, which answers `rows::Listing` — so swapping which side
        /// of the seam answered was the whole of it.
        ///
        /// Two helpers dissolved on the way. `recording_of` *is*
        /// `Listing::recording_of`, and `holdings_here` is `rows()` folded by
        /// it. `audio_of` left the trait entirely: it existed so a caller
        /// could take one hop through each arena, and there is one arena.
        ///
        /// Like `REACHES`, this is a floor and not a ratchet. What is left to
        /// catch is a mention appearing.
        const STORE: usize = 0;
        /// **What a `PathBuf` in the player is allowed to be.**
        ///
        /// Counting them was the wrong measure and it had reached its floor
        /// without anyone noticing. All four that remain are legitimate, and
        /// two are not track paths at all:
        ///
        /// - `session_path` and `data_dir` are the **player's own files**.
        ///   Nothing about "the library must not be keyed by a path" says the
        ///   program may not know where it keeps its own state.
        /// - `PathBuf::from(locator)` is the sanctioned production point —
        ///   §"the locator is produced here and used immediately", and the
        ///   whole discipline is that it is not *kept*.
        /// - the `use` itself.
        ///
        /// So the count could only ever fall to four, and a number with a
        /// floor nobody had written down is a number somebody will chase.
        /// Replaced by the question it was standing in for: **is the player
        /// holding a path that is neither its own file nor one it is opening
        /// right now?** That fails on a new one and stays silent on these,
        /// which is what the ratchet was for.
        const SANCTIONED_PATHS: &[&str] = &[
            "use std::path",
            "session_path",
            "data_dir",
            "PathBuf::from(locator)",
        ];
        /// **The number that actually measures the seam.** The player reaches
        /// through `ctx.store` far more often than it names the type, so
        /// counting `Scan` alone said 5 while sixteen reads went around the
        /// librarian. Replacing a read lowers this and leaves the others flat,
        /// which is exactly the progress #77 is made of.
        ///
        /// One went when playback started opening through the librarian
        /// (#85): the locator and the recording behind it are asked of the
        /// library now, and what remains of the store on that path is reached
        /// through `rows::Listing` rather than by name.
        ///
        /// Three more went with #77's first pass — a rating's recording, the
        /// underrun label, and seeking by percentage. **What the last eleven
        /// are waiting on is not effort but three other decisions**, and they
        /// are listed here so the next person does not re-derive them:
        ///
        /// The five that rendered through `format::Template` are gone: it
        /// takes a `rows::Listed` now, which answers fourteen of the sixteen
        /// fields it used to read off a `Track`. The two it does not are
        /// `path` and `file_name`, and a template asking for either is refused
        /// when it is parsed — a row carries no location (#78).
        /// The session's two went with #112 — it names a recording and the
        /// copy that was open, so resuming reopens the file that was open
        /// rather than whichever the canonical rule would pick now.
        ///
        /// **Zero** (#86), and this is no longer a ratchet but a floor.
        ///
        /// The last two went together. The remote's snapshot is a
        /// `rows::Drawn` — an owned row, which is the one thing a borrow could
        /// not do and the whole reason it reached for the record itself. And
        /// reading an `.m3u` asks `Listing::at`, the inverse of `playable`:
        /// the one question that has to start from a path, because an `.m3u`
        /// is a path format. What the playlist holds afterwards is row ids.
        ///
        /// Before them, #86's second pass took the filter and the sort, and
        /// its first pass took the two `.m3u` writes.
        ///
        /// It cannot fall further, so a rise is the only thing left to catch —
        /// which is what this now does.
        const REACHES: usize = 0;

        let (mut store, mut reaches) = (0, 0);
        let mut stray_paths: Vec<String> = Vec::new();
        // **The UI is in the binary crate**, one level up from this one. Named
        // relatively because that is the truth and because `read` now fails
        // loudly if it stops being true, which is what this whole comment is
        // about.
        for (name, text) in read("../../src/ui") {
            let live = code_only(production(&text));
            store += identifiers(&live, "Scan");
            reaches += live.matches("ctx.store.").count();
            for line in live.lines().filter(|l| l.contains("PathBuf")) {
                if !SANCTIONED_PATHS.iter().any(|ok| line.contains(ok)) {
                    stray_paths.push(format!("{name}: {}", line.trim()));
                }
            }
        }

        assert!(
            stray_paths.is_empty(),
            "the player is holding a path that is not one of its own files, \
             and not one produced at the moment of opening: {stray_paths:#?}"
        );
        // **An assertion, not a ratchet.** It reached zero, and a number that
        // cannot fall further has nothing left to ratchet — what is worth
        // catching is a new one appearing.
        assert_eq!(
            reaches, REACHES,
            "the player reached past the seam again: {reaches} time(s)"
        );

        assert_eq!(
            store, STORE,
            "the player named the store again: {store} time(s)"
        );
    }

    /// **An assignment is identified by what it says, never by an id.**
    ///
    /// `entity.rs` lists `GenreAssignment` as a kind, and it stays one so
    /// anything derived from an assignment can be seeded by `KIND`. But
    /// minting `Id<GenreAssignment>` would put a *third* identity in the model
    /// — beside the subject's and the concept's — and it could only be one of
    /// two things. Random breaks convergent minting outright: two devices
    /// importing the same Discogs page have to emit the same row, and random
    /// ids guarantee they cannot. Content-derived buys nothing, because the
    /// content is already the key.
    ///
    /// The cost is not theoretical. An assignment with its own id is two rows
    /// that must cross the airgap together, and a fold that has to join across
    /// fact families before it can decide whether a claim is live.
    ///
    /// Asserted rather than remembered, because the reflex when a relationship
    /// grows a field is to reify it into a node, and here the field belongs on
    /// the row.
    #[test]
    fn no_assignment_carries_an_id() {
        let minted: Vec<_> = read("src")
            .into_iter()
            .filter(|(_, text)| {
                let live = production(text);
                live.contains("Id<GenreAssignment>") || live.contains("Id<QualifierAssignment>")
            })
            .map(|(path, _)| path)
            .collect();
        assert!(
            minted.is_empty(),
            "an assignment grew an identity of its own: {minted:#?}"
        );
    }

    /// **A group with no pressing fact yields no pressing.**
    ///
    /// `from_catalogue` used to mint a `Release` for every group and a
    /// `Medium` for every distinct disc number it saw, so those rows existed
    /// whether or not anyone had said so — with no stamp, no author and no way
    /// to correct one, and indistinguishable downstream from a folded fact.
    ///
    /// Asserted from the empty side because that is the side that cannot lie:
    /// a projection that invents will produce a row here, and one that reads
    /// cannot.
    #[test]
    fn the_projection_reads_the_spine_and_does_not_invent_it() {
        use crate::projection::catalogue::{Catalogue, Grouped};

        let mut cat = Catalogue::default();
        let group = Id::<crate::model::entity::ReleaseGroup>::derived(b"luna");
        cat.groups.insert(
            group,
            Grouped {
                title: "Luna".into(),
                artist: Id::derived(b"bodzin"),
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
        );

        // And a recording, so the credit side has something to invent *for*.
        let rec = Id::<Recording>::derived(b"callisto");
        cat.recordings.insert(
            rec,
            crate::projection::catalogue::Recorded {
                title: "Callisto".into(),
                artist: Id::derived(b"bodzin"),
            },
        );

        let l = Librarian::from_catalogue(&cat);
        assert_eq!(l.spine.groups.len(), 1, "the album was read");
        assert_eq!(l.spine.recordings.len(), 1, "the recording was read");
        assert!(l.spine.releases.is_empty(), "a pressing nobody asserted");
        assert!(l.spine.media.is_empty(), "a disc nobody asserted");
        assert!(l.spine.tracks.is_empty(), "a slot nobody asserted");
        assert!(l.spine.works.is_empty(), "a composition nobody asserted");
        assert!(
            l.reference.credits.is_empty(),
            "every artist was a performer because one line said so"
        );
        assert!(
            l.reference.roles.is_empty(),
            "a codelist entry nobody defined"
        );
    }

    /// **A compilation does not decide a track's genre, even when the track has none**
    /// — #74, #253.
    ///
    /// The rule was already half here: a row takes the recording's own concepts and
    /// falls back to the album's only where the recording carries none. What it fell
    /// back to was **the group of the slot being drawn**, so a recording with no genre
    /// of its own, held on both its album and a compilation, showed `Pop` when drawn
    /// through the compilation.
    ///
    /// That is the case `Vocabulary::effective_genres` was written and tested for, and
    /// #253 is that it was never called by anything. This asserts the rule where the
    /// read actually happens.
    ///
    /// **The shape is a supported one**, which is why it matters: holding the same
    /// recording on an album and on a compilation is what `shelf copies` exists to
    /// describe rather than resolve.
    #[test]
    fn a_compilation_does_not_name_a_track_it_only_carries() {
        use crate::model::facet::{ConceptRecord, GENRE};
        use crate::model::spine::ReleaseSecondary;
        use crate::projection::catalogue::{
            Catalogue, Grouped, Placed, Pressed, Recorded, Released,
        };

        let artist = Id::derived(b"the-artist");
        let mut cat = Catalogue::default();

        // The album, and the compilation that also carries the track.
        let album = Id::<ReleaseGroup>::derived(b"the-album");
        let comp = Id::<ReleaseGroup>::derived(b"80s-hits");
        cat.groups.insert(
            album,
            Grouped {
                title: "The Album".into(),
                artist,
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
        );
        cat.groups.insert(
            comp,
            Grouped {
                title: "80s Hits".into(),
                artist,
                first_released: None,
                kind: Default::default(),
                secondary: [ReleaseSecondary::Compilation].into_iter().collect(),
            },
        );

        let rec = Id::<Recording>::derived(b"the-song");
        cat.recordings.insert(
            rec,
            Recorded {
                title: "The Song".into(),
                artist,
            },
        );

        // A pressing, a disc and a slot for each group, so the recording really sits
        // in both.
        let slot_on = |cat: &mut Catalogue, group: Id<ReleaseGroup>, tag: &[u8]| {
            let release = Id::<crate::model::entity::Release>::derived(tag);
            cat.releases.insert(
                release,
                Released {
                    group,
                    title: "A Pressing".into(),
                    musicbrainz: None,
                    released: None,
                    label: None,
                    media: Some(1),
                },
            );
            let medium = Id::<crate::model::entity::Medium>::derived(tag);
            cat.media.insert(
                medium,
                Pressed {
                    release,
                    position: 1,
                    format: None,
                    tracks: Some(1),
                },
            );
            let track = Id::<Track>::derived(tag);
            cat.tracks.insert(
                track,
                Placed {
                    medium,
                    recording: rec,
                    position: 1,
                    title_on_release: None,
                },
            );
            track
        };
        let _on_album = slot_on(&mut cat, album, b"album-slot");
        let on_comp = slot_on(&mut cat, comp, b"comp-slot");

        // Rock on the album, Pop on the compilation, and nothing on the recording.
        let facet = Id::of_key(GENRE);
        let genre = |cat: &mut Catalogue, label: &str, subject: Described| {
            let concept = Id::<crate::model::entity::Concept>::in_facet(GENRE, label);
            cat.concepts.insert(
                concept,
                ConceptRecord {
                    facet,
                    label: label.into(),
                    node_label: false,
                },
            );
            cat.assertions.insert(
                (subject, concept, crate::model::reference::Source::Manual),
                crate::model::ingest::Confidence::Confirmed,
            );
        };
        genre(&mut cat, "Rock", Described::ReleaseGroup(album));
        genre(&mut cat, "Pop", Described::ReleaseGroup(comp));

        let l = Librarian::from_catalogue(&cat);

        // **The instrument is attached.** Every assertion below is about which of two
        // genres a row shows, and it means nothing if the fixture produced neither.
        assert_eq!(l.spine.tracks.len(), 2, "the recording is not on both");
        assert!(
            l.concepts
                .contains_key(&(Described::ReleaseGroup(album), facet)),
            "the album has no genre, so there is nothing for the rule to prefer"
        );
        assert!(
            l.concepts
                .contains_key(&(Described::ReleaseGroup(comp), facet)),
            "the compilation has no genre, so nothing could be wrongly preferred"
        );
        assert!(
            !l.concepts.contains_key(&(Described::Recording(rec), facet)),
            "the recording has a genre of its own, so the fallback is never reached"
        );

        let drawn = l
            .drawn_of(
                crate::model::rows::CopyId::On(on_comp),
                &Default::default(),
                &Lookup::Direct(&l),
            )
            .expect("a row through the compilation");
        assert_eq!(
            drawn.listed().on(facet),
            ["Rock"],
            "the compilation named a track it only carries"
        );
    }

    /// **No tool authors a fact that looks reviewed.**
    ///
    /// Two routes write to the library and they are not equals. A person
    /// acting on one thing in the UI appends directly, with `Source::Manual`
    /// and `Confidence::Confirmed`, because the person *is* the review. A
    /// tool — a matcher, an import, any pass in `tools/` — writes a proposal
    /// file, and `apply` turns it into records only after somebody has read
    /// it.
    ///
    /// Volume is the whole reason. A human mis-tags one recording; a matching
    /// pass mis-tags six hundred in a minute, and the three of 607 that this
    /// library's identification pass got wrong were all found by reading the
    /// file. A tool that could write `Manual` would be laundering its guesses
    /// into the one basis nothing downstream second-guesses.
    ///
    /// Checked at the boundary rather than in the type system: `Source` has to
    /// stay one enum so that provenance is one mechanism, so what stops the
    /// wrong variant is this test.
    #[test]
    fn no_tool_facing_surface_claims_a_person_said_so() {
        // `apply.rs` is the crossing point and is allowed to name it: a
        // proposal file a person edited by hand may say `manual`, which is
        // that person asserting, not the tool.
        let culprits: Vec<_> = read("src")
            .into_iter()
            .filter(|(path, _)| {
                path.contains("ingest.rs") || path.contains("scan.rs") || path.contains("tools")
            })
            .filter(|(_, text)| production(text).contains("Source::Manual"))
            .map(|(path, _)| path)
            .collect();
        assert!(
            culprits.is_empty(),
            "a tool wrote the basis reserved for a person: {culprits:#?}"
        );
    }

    /// **Every fact names its subject.**
    ///
    /// `[Subject][PastTenseVerb]` is the event-sourcing convention, and four
    /// variants used to break it — `Measured`, `Rated`, `Played`, `Acquired`.
    /// Each dropped the one word that mattered: `Measured` measured *what*? An
    /// encoding, not a recording, which is the distinction this whole model
    /// exists to draw.
    ///
    /// Asserted rather than remembered, because the next variant will be
    /// written by someone reaching for the shortest name that compiles.
    #[test]
    fn every_fact_names_its_subject() {
        const SUBJECTS: &[&str] = &[
            // Not one of the eight kinds an alias can hang off, and that is
            // the point: like `Holdings`, the subject is the thing being
            // recorded rather than the entity it attaches to. `ArtistAliased`
            // would need seven siblings saying nothing the `Cataloguable`
            // does not already say.
            "Alias",
            "Artist",
            "Collection",
            "Credit",
            "Device",
            "Encoding",
            "Concept",
            "Facet",
            // On the list for the same reason `Alias` is, and it is the
            // authority-control word: what a `HeadingRetired` records is the
            // heading, and the eight things a heading can belong to are
            // already the `Cataloguable`.
            "Heading",
            "Hierarchy",
            "Holdings",
            "Label",
            "Medium",
            "Release",
            "ReleaseGroup",
            "Role",
            "Recording",
            "Track",
            "View",
            "Work",
        ];

        let (_, text) = read("src/facts")
            .into_iter()
            .find(|(f, _)| f.ends_with("mod.rs"))
            .expect("the facts module");
        let start = text.find("pub enum Fact {").expect("the Fact enum");
        let body = &text[start..text[start..].find("\n}\n").unwrap() + start];

        let mut nameless = Vec::new();
        for line in body.lines() {
            let Some(name) = line.strip_prefix("    ").and_then(|l| l.split(' ').next()) else {
                continue;
            };
            if !name.starts_with(char::is_uppercase) || !line.ends_with('{') {
                continue;
            }
            if !SUBJECTS.iter().any(|s| name.starts_with(s)) {
                nameless.push(name.to_string());
            }
        }
        assert!(
            nameless.is_empty(),
            "these facts do not say what they are about: {nameless:?}"
        );
    }

    /// Everything before the file's test module.
    ///
    /// Crude on purpose: this codebase puts `#[cfg(test)]` last and once per
    /// file, so a split on the first occurrence is exact here and obvious when
    /// it stops being. A parser would be more correct and less readable, and
    /// what is being measured is a trend rather than a total.
    fn production(text: &str) -> &str {
        match text.find("#[cfg(test)]") {
            Some(at) => &text[..at],
            None => text,
        }
    }

    /// Production code with its comments removed.
    ///
    /// **The ratchet used to count prose (#113).** It matched substrings over
    /// the whole file, so a doc comment explaining what the player must stop
    /// doing counted as the player doing it — and the closer the explanation
    /// got to naming the thing, the worse the number looked. Two of the
    /// `Store` mentions and one `PathBuf` were sentences.
    fn code_only(text: &str) -> String {
        text.lines()
            .map(|line| match line.find("//") {
                // Not inside a string literal: `"//"` is a path separator, not
                // a comment, and cutting there would silently drop real code.
                Some(at) if line[..at].matches('"').count() % 2 == 0 => &line[..at],
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// How many times `name` appears as an identifier rather than as a
    /// fragment of a longer one.
    ///
    /// `Scan` is inside `Scanned` and `Store` inside `Stored`, and counting
    /// the substring meant the number could not reach zero however much moved
    /// (#113).
    fn identifiers(text: &str, name: &str) -> usize {
        let bytes = text.as_bytes();
        let part = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        text.match_indices(name)
            .filter(|(at, _)| {
                let before = *at == 0 || !part(bytes[at - 1]);
                let after = at + name.len() >= bytes.len() || !part(bytes[at + name.len()]);
                before && after
            })
            .count()
    }

    /// **The ratchet's own arithmetic, which the library does not exercise.**
    ///
    /// Both refinements survive mutation against `src/ui` — degrade
    /// `identifiers` to a substring match, or let `code_only` cut inside a
    /// string literal, and the count does not move, because no file happens
    /// to contain the case. A guard the corpus does not reach is a guard
    /// nobody is testing, so it is tested here instead.
    #[test]
    fn the_ratchet_counts_identifiers_and_not_prose() {
        // The reason the count could not reach zero: `Scan` is inside
        // `Scanned`, and `Store` was inside `Stored`.
        assert_eq!(
            identifiers("Scanned rescan SCAN", "Scan"),
            0,
            "a longer name that starts with it"
        );
        assert_eq!(
            identifiers("PreScan pre_Scan", "Scan"),
            0,
            "and one that ends with it"
        );
        assert_eq!(identifiers("let s: Scan = Scan::new();", "Scan"), 2);
        assert_eq!(
            identifiers("model::Scan,", "Scan"),
            1,
            "punctuation is a boundary"
        );
        assert_eq!(
            identifiers("Scan", "Scan"),
            1,
            "and so is the end of the text"
        );

        // A doc comment explaining what the player must stop doing is not the
        // player doing it.
        assert_eq!(
            identifiers(&code_only("// a comment about Scan"), "Scan"),
            0
        );
        assert_eq!(identifiers(&code_only("/// Scan, in prose"), "Scan"), 0);
        assert_eq!(
            identifiers(&code_only("let x: Scan = y; // and Scan again"), "Scan"),
            1
        );

        // And `//` inside a string is a path separator. Cutting there would
        // drop real code and quietly lower the number.
        assert_eq!(
            identifiers(
                &code_only(r#"let u = "http://x"; let s: Scan = y;"#),
                "Scan"
            ),
            1
        );
    }

    /// **The hierarchy orders a row's genres, broadest first** (#116).
    ///
    /// A row carries every genre assigned to it and a line has one slot, so
    /// which comes first is the whole question. Tag order does not survive the
    /// fold — the catalogue keys assignments by `(subject, genre, source)`, a
    /// set with no position — and alphabetical was the stand-in, which put
    /// `Abstract` where the tag had put `Rock`.
    ///
    /// This is what makes running `gmuse-tools genre apply` worth something:
    /// the live log contains no hierarchy at all, so every genre is depth 0
    /// today and the order falls back to the name.
    #[test]
    fn a_rows_genres_are_ordered_broadest_first() {
        use crate::model::entity::Hierarchy;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        i.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 9 },
                title: "Racing to a Red Light".into(),
                artist: "Early James".into(),
                album: Some("Strange Time".into()),
                // Deliberately in an order that is neither the tags' usual one
                // nor alphabetical, so neither can pass by accident.
                genres: vec![
                    "Blues Rock".into(),
                    "Alternative Rock".into(),
                    "Rock".into(),
                ],
                ..Default::default()
            }],
        );

        // `Rock` is the top concept, `Alternative Rock` is narrower-of it, and
        // `Blues Rock` narrower again — so depth does more here than "is it a
        // top concept", and alphabetical would give the opposite order.
        let hierarchy = Id::<Hierarchy>::derived(b"discogs");
        i.narrows(200, hierarchy, facet::GENRE, "Rock", "Alternative Rock");
        i.narrows(
            200,
            hierarchy,
            facet::GENRE,
            "Alternative Rock",
            "Blues Rock",
        );

        let l = Librarian::from_catalogue(&i.catalogue());
        let row = *l.rows.keys().next().expect("a row");
        let shown = l.describe(row).expect("a row").genre().to_vec();

        assert_eq!(
            shown,
            ["Rock", "Alternative Rock", "Blues Rock"],
            "the genres were not ordered by how broad they are"
        );
    }

    /// **The aliases reach the row, which they never did** — #179.
    ///
    /// `cat.aliases` has held them since the fold learned `AliasAsserted`, and
    /// `Reference::aliases` is the field they belong in, and nothing carried
    /// one to the other. Every reader below `from_catalogue` therefore saw a
    /// library with no aliases in it — including the single one this library
    /// actually holds, the `Campfire Songs` an Animal Collective merge
    /// retired.
    ///
    /// **And a moniker is deliberately not followed.** A name of the same
    /// identity is findable; a *related* identity is a different question, and
    /// #174 gave it a different sentence. `artist="AFX"` finds what was
    /// released as AFX and must not pull in the rest of Aphex Twin.
    #[test]
    fn a_row_carries_the_other_names_its_artists_answer_to() {
        use crate::model::instance::{Instance, Scanned};
        use crate::model::reference::{AliasKind, ArtistRelationKind};

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        i.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 7 },
                title: "Queen in My Pictures".into(),
                artist: "Animal Collective".into(),
                album: Some("Campfire Songs".into()),
                ..Default::default()
            }],
        );

        let before = Librarian::from_catalogue(&i.catalogue());
        let artist = before
            .reference
            .artist_named("Animal Collective")
            .expect("the ingest named one");
        let row_id = *before.rows.keys().next().expect("one row");
        assert_eq!(
            before.describe(row_id).expect("a row").credited,
            [artist],
            "the row should credit the artist the ingest named, and only them"
        );

        // The heading a merge retired, a hidden search hint, and a moniker —
        // a *separate* identity, related rather than renamed.
        let other = Id::<crate::model::entity::Artist>::derived(b"avey tare");
        for fact in [
            crate::facts::Fact::HeadingRetired {
                subject: crate::model::reference::Cataloguable::Artist(artist),
                value: "Campfire Songs".into(),
            },
            crate::facts::Fact::AliasAsserted {
                subject: crate::model::reference::Cataloguable::Artist(artist),
                value: "animalcollective".into(),
                kind: AliasKind::SearchHint,
            },
            crate::facts::Fact::ArtistNamed {
                artist: other,
                name: "Avey Tare".into(),
            },
            crate::facts::Fact::ArtistRelated {
                from: other,
                to: artist,
                kind: ArtistRelationKind::MemberOf,
                period: None,
            },
        ] {
            i.append_fact(200, fact);
        }

        let l = Librarian::from_catalogue(&i.catalogue());
        let row = l.describe(row_id).expect("a row");

        // **The row did not change, and that is the point** (#193). Four facts
        // arrived, two of them about what this artist is called, and the row
        // still says exactly what it said — because it holds an id and never
        // held a name to go stale.
        assert_eq!(
            row.credited,
            [artist],
            "a name changing must not move a row"
        );

        // The names are asked of the library, which is the one place they live.
        let names = crate::query::filter::Named::names_of(&l, artist);
        assert!(
            names.iter().any(|n| n == "Campfire Songs"),
            "the retired heading is not findable: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "animalcollective"),
            "nor is the hidden name, which is the one a filter is for"
        );
        assert!(
            names.iter().any(|n| n == "Animal Collective"),
            "and its own name is among them, because this is every name it goes by"
        );

        // **The moniker is a row of its own, not a name of this one.**
        assert!(
            !names.iter().any(|n| n == "Avey Tare"),
            "a related identity is a different question (#174): {names:?}"
        );
        assert_eq!(
            row.artist,
            Some("Animal Collective"),
            "and what it prints is unchanged"
        );
    }

    /// **A file that names no album has no position and no date.**
    ///
    /// A track number is a position *on a medium of a release* and a date is a
    /// fact about a pressing, so a file whose tags name neither gets neither.
    /// A flat `Track` read both straight off the tag; the containment says
    /// otherwise, and "track 3 of nothing" is not a fact (#116).
    ///
    /// Pinned rather than left implicit because it is invisible when it bites:
    /// a loose single would show a blank where a year used to be, and nobody
    /// would know to look. All 6,500 files in the real library carry an album
    /// tag, so nothing is affected today — which is exactly why a test has to
    /// be the thing that notices if that stops being true.
    #[test]
    fn a_file_with_no_album_has_no_track_number_or_year() {
        use crate::model::instance::{Instance, Scanned};

        let loose = Scanned {
            audio: crate::audio_id::AudioId { scheme: 1, hash: 5 },
            title: "A Loose Single".into(),
            artist: "Somebody".into(),
            // The tags say both, and there is nowhere for either to live.
            track: Some(3),
            released: Some(crate::model::spine::PartialDate::year(2024)),
            album: None,
            ..Default::default()
        };
        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        i.ingest(100, std::slice::from_ref(&loose));
        let l = Librarian::from_catalogue(&i.catalogue());

        let row_id = *l.rows.keys().next().expect("one row");
        let row = l.describe(row_id).expect("a row");
        assert_eq!(row.title, "A Loose Single", "it is still a row");
        assert_eq!(row.track, None, "a position needs something to be on");
        assert_eq!(row.disc, None);
        assert_eq!(row.year, None, "a date needs an edition to be about");
    }

    /// **A tag's genres become facts, and a drawn row carries all of them.**
    ///
    /// The librarian showed no genre at all for 6,437 of 6,485 real rows,
    /// because a scan observed none: genres in this model are concepts with a
    /// basis and a confidence, and a tag had never been read as a claim. It is
    /// one now, at `TagVariant` — the weakest rung, which a curated pass
    /// outranks.
    #[test]
    fn the_genres_a_tag_names_reach_the_row() {
        use crate::model::entity::Device;
        use crate::model::instance::Instance;

        let dir = std::env::temp_dir().join("gmuse-seam-genres");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("01 Racing.wav");
        crate::model::round_trip::write_wav(&file, 4410);
        crate::model::round_trip::write_genre(&file, "Rock; Blues Rock; Alternative Rock");

        let scan = vec![crate::model::ingest::read(&file).expect("reads")];
        assert_eq!(scan[0].genres.len(), 3, "the scan saw three terms");

        let device = Id::<Device>::derived(b"laptop");
        let mut m = Instance::new(device);
        m.ingest(100, &scan);
        let l = Librarian::from_catalogue(&m.catalogue());

        let row_id = *l.rows.keys().next().unwrap();
        let row = l.describe(row_id).expect("a row");
        let mut shown: Vec<&str> = row.genre().iter().map(String::as_str).collect();
        shown.sort_unstable();
        assert_eq!(
            shown,
            ["Alternative Rock", "Blues Rock", "Rock"],
            "every term, not the first and not the joined string"
        );
    }

    /// A row is assembled by the library and carries no location, so the
    /// player cannot key anything by one.
    #[test]
    fn a_row_is_made_of_identities_and_values() {
        use crate::model::spine::Recording as Rec;
        use std::time::Duration;

        let mut l = Librarian::new();
        let id = Id::<Recording>::derived(b"callisto");
        l.spine.recordings.insert(
            id,
            Rec {
                id,
                title: "Callisto".into(),
                work: None,
                recorded: None,
                bpm: None,
                length: Duration::from_secs(400),
                pinned: None,
            },
        );

        let row = l.row(id).expect("a catalogued recording has a row");
        assert_eq!(row.title, "Callisto");
        assert!(!row.available, "no encoding, and still an ordinary row");
        assert_eq!(row.format, None);
    }

    /// **End to end.** Real files on disk, through ingest, through a log, to
    /// rows on a screen — with two machines and a sync in the middle, because
    /// that is the shape this whole layer exists to support.
    #[test]
    fn files_become_rows_by_way_of_a_log() {
        use crate::model::entity::Device;
        use crate::model::instance::Instance;

        let dir = std::env::temp_dir().join("gmuse-seam-end-to-end");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("03 Callisto.wav");
        crate::model::round_trip::write_wav(&file, 4410);
        crate::model::round_trip::write_tags(
            &file,
            "Callisto",
            "Stephan Bodzin & Marc Romboy",
            Some("183105b5-0000-4000-8000-000000000000"),
        );

        let scan = vec![crate::model::ingest::read(&file).expect("reads")];
        let (mut m1, mut m2) = (
            Instance::new(Id::<Device>::derived(b"laptop")),
            Instance::new(Id::<Device>::derived(b"nas")),
        );
        m1.ingest(100, &scan);
        m2.ingest(140, &scan);

        let rec = *m1.catalogue().recordings.keys().next().unwrap();
        m1.rate(200, rec, Some(4));
        m2.play(240, rec, *m2.catalogue().encodings.keys().next().unwrap());

        let from_m2 = m2.records().clone();
        m1.merge(300, &from_m2);

        let l = Librarian::from_catalogue(&m1.catalogue());
        let row = l.row(rec).expect("a row");

        assert_eq!(row.title, "Callisto");
        assert_eq!(row.file_under, "Stephan Bodzin & Marc Romboy");
        assert_eq!(row.rating, Some(4), "set on the laptop");
        assert_eq!(row.plays, 1, "played on the NAS");
        assert!(row.available);
    }

    /// **Splitting an album credit must not empty the line** — #152.
    ///
    /// Written after a dry run proposed moving 288 files into `Unknown
    /// Artist`. `file_under` reads the release group's credit line, so a
    /// repair that leaves it blank is worse than the joined name it replaced.
    #[test]
    fn splitting_an_album_credit_names_the_artists_it_split_into() {
        use crate::model::instance::{Instance, Scanned};

        let mut log = Instance::new(Id::<Device>::derived(b"laptop"));
        log.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 7 },
                title: "Callisto".into(),
                artist: "Stephan Bodzin & Marc Romboy".into(),
                album: Some("Luna".into()),
                album_artist: Some("Stephan Bodzin & Marc Romboy".into()),
                ..Default::default()
            }],
        );
        let cat = log.catalogue();
        let rec = *cat.recordings.keys().next().unwrap();
        let group = *cat.groups.keys().next().expect("an album makes a group");

        let before = Librarian::from_catalogue(&cat).row(rec).expect("a row");
        assert_eq!(before.file_under, "Stephan Bodzin & Marc Romboy");

        // The repair: credit the two artists the joined string names, at a
        // basis that supersedes the tag's reading.
        for (at, who) in [(0u8, "Stephan Bodzin"), (1, "Marc Romboy")] {
            let artist = Id::<crate::model::entity::Artist>::of_artist(None, who);
            log.rename_artist(200, artist, who);
            log.credit(
                200,
                CreditOn::ReleaseGroup(group),
                artist,
                "performer",
                crate::model::reference::Source::MusicBrainz,
                at,
            );
        }

        let after = Librarian::from_catalogue(&log.catalogue())
            .row(rec)
            .expect("a row");
        assert_eq!(
            after.file_under, "Stephan Bodzin, Marc Romboy",
            "the split names both, and never nobody"
        );
    }

    /// A file with no title tag still shows something useful, and the row
    /// never has to reach for a path to find it — ingest already did.
    #[test]
    fn a_title_never_falls_back_to_a_path_at_display_time() {
        use crate::model::entity::Device;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"d"));
        i.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 7 },
                title: "04 Unknown Thing".into(), // what ingest put there
                artist: String::new(),
                ..Default::default()
            }],
        );
        let cat = i.catalogue();
        let rec = *cat.recordings.keys().next().unwrap();
        let l = Librarian::from_catalogue(&cat);

        let row = l.row(rec).unwrap();
        assert_eq!(row.title, "04 Unknown Thing");
        assert_eq!(
            row.file_under, UNKNOWN_ARTIST,
            "nothing named the artist, and that is an ordinary row"
        );
    }

    /// **A bad copy is a row with a verdict, not a folder** — issue #87.
    ///
    /// 726 files sit in `~/LowQualityMusic` because the library had one grade
    /// of membership: "this needs replacing" could only be said by taking it
    /// out. A folder cannot express *I have this, it is not good enough, and I
    /// want a better one* — only *I have this* or *I do not*.
    ///
    /// With the measurement in the model it is a filter, and the track is
    /// still there to play in the meantime.
    #[test]
    fn a_transcode_is_a_row_with_a_verdict() {
        use crate::model::encoding::{Format, Verdict};
        use crate::model::entity::Device;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        let lying = crate::audio_id::AudioId {
            scheme: 1,
            hash: 0xbad,
        };
        let honest = crate::audio_id::AudioId {
            scheme: 1,
            hash: 0x600d,
        };
        i.ingest(
            100,
            &[
                Scanned {
                    audio: lying,
                    title: "Isn't Love".into(),
                    artist: "Someone".into(),
                    format: Some(Format::Mp3),
                    bitrate: 320,
                    ..Default::default()
                },
                Scanned {
                    audio: honest,
                    title: "Fine".into(),
                    artist: "Someone".into(),
                    format: Some(Format::Mp3),
                    bitrate: 320,
                    ..Default::default()
                },
            ],
        );

        // Before anything is measured, nothing can be said.
        let before = Librarian::from_catalogue(&i.catalogue());
        assert!(
            before
                .encodings
                .encodings
                .values()
                .all(|e| e.verdict() == Verdict::Unmeasured),
            "a verdict without a measurement would be a guess"
        );

        // A quality pass runs — separately, because an FFT over a library is
        // minutes where hashing is seconds.
        let (bad, good) = (Id::of_audio(&lying), Id::of_audio(&honest));
        i.measured(
            200,
            bad,
            15_700,
            Some(5_200),
            crate::model::encoding::ANALYSER,
        );
        i.measured(
            200,
            good,
            19_800,
            Some(58),
            crate::model::encoding::ANALYSER,
        );

        let l = Librarian::from_catalogue(&i.catalogue());
        assert_eq!(
            l.encodings.encodings[&bad].verdict(),
            Verdict::Suspect,
            "320k claimed, 15.7 kHz measured — a lossy source re-encoded upward"
        );
        assert_eq!(l.encodings.encodings[&good].verdict(), Verdict::Good);

        // The re-buy list is a filter over rows that are still there, not a
        // directory listing of things that are not.
        let rebuy: Vec<_> = l
            .recordings()
            .filter_map(|r| l.row(r))
            .filter(|row| row.verdict == Some(Verdict::Suspect))
            .collect();
        assert_eq!(rebuy.len(), 1);
        assert_eq!(rebuy[0].title, "Isn't Love");
        assert!(
            rebuy[0].available,
            "and it still plays while you decide what to do about it"
        );
    }

    /// A re-measure targets what the old analyser touched, so a changed FFT
    /// does not mean redoing the library.
    #[test]
    fn an_older_measurement_is_findable() {
        use crate::model::encoding::{ANALYSER, Format};
        use crate::model::entity::Device;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        let a = crate::audio_id::AudioId { scheme: 1, hash: 1 };
        i.ingest(
            100,
            &[Scanned {
                audio: a,
                title: "x".into(),
                artist: "y".into(),
                format: Some(Format::Mp3),
                bitrate: 320,
                ..Default::default()
            }],
        );
        i.measured(200, Id::of_audio(&a), 19_000, None, ANALYSER - 1);

        let l = Librarian::from_catalogue(&i.catalogue());
        assert_eq!(l.encodings.stale_measurements(), vec![Id::of_audio(&a)]);
    }

    /// A recording nothing holds is still a row — decided about, not owned.
    #[test]
    fn something_not_owned_is_not_a_missing_row() {
        let l = Librarian::new();
        assert!(l.row(Id::derived(b"never-catalogued")).is_none());
    }
}

#[cfg(test)]
mod collections {
    //! **A playlist is a sequence of facts** — #92, and `domain-model.md`
    //! §"Collections".
    //!
    //! A collection and a playlist are one entity; a playlist is a collection
    //! whose order means something. What makes it a *sequence* rather than a
    //! set is that it admits duplicates and carries an order, and both of
    //! those have to survive a merge — which is what these check.

    use super::*;
    use crate::model::entity::Collection;
    use crate::model::instance::Instance;
    use crate::model::ord::OrdKey;

    fn instance(who: &[u8]) -> Instance {
        Instance::new(Id::<Device>::derived(who))
    }

    fn list() -> Id<Collection> {
        Id::<Collection>::derived(b"road trip")
    }

    fn song(n: u8) -> Id<Recording> {
        Id::<Recording>::derived(&[n])
    }

    /// Append `n` songs, and give back the keys so a test can insert between.
    fn appended(i: &mut Instance, songs: &[Id<Recording>]) -> Vec<OrdKey> {
        let mut keys: Vec<OrdKey> = Vec::new();
        for r in songs {
            let k = OrdKey::between(keys.last(), None);
            i.add_to_collection(100, list(), *r, k.clone());
            keys.push(k);
        }
        keys
    }

    fn shelved(i: &Instance) -> Vec<Id<Recording>> {
        Librarian::from_catalogue(&i.catalogue()).entries_of(list())
    }

    /// The ordinary case, and the one everything else is measured against.
    #[test]
    fn a_collection_comes_back_in_the_order_it_was_built() {
        let mut i = instance(b"laptop");
        i.name_collection(100, list(), "Road Trip", true);
        appended(&mut i, &[song(1), song(2), song(3)]);

        assert_eq!(shelved(&i), [song(1), song(2), song(3)]);
        assert_eq!(
            Librarian::from_catalogue(&i.catalogue()).collections()[&list()].name,
            "Road Trip"
        );
    }

    /// **The same recording twice is two entries.**
    ///
    /// The property that makes a view a sequence rather than a predicate. A
    /// set keyed by `(collection, recording)` could not say this, and playing
    /// one song twice in a queue is ordinary rather than exotic — which is why
    /// #92 says "the queue is just a view" is false unless a view can be one.
    #[test]
    fn one_recording_twice_is_two_entries() {
        let mut i = instance(b"laptop");
        appended(&mut i, &[song(1), song(2), song(1)]);
        assert_eq!(
            shelved(&i),
            [song(1), song(2), song(1)],
            "the duplicate was folded away"
        );
    }

    /// Inserting between two entries touches no other row — the whole reason
    /// a position is a fractional index and not an integer.
    #[test]
    fn inserting_between_two_entries_moves_nothing_else() {
        let mut i = instance(b"laptop");
        appended(&mut i, &[song(1), song(3)]);
        let before = i.catalogue().entries.clone();

        // Asked of the library rather than remembered by the caller, which is
        // how an edit will do it: the two keys it is landing between are what
        // it needs, and nothing else.
        let at = Librarian::from_catalogue(&i.catalogue()).positions_of(list());
        let mid = OrdKey::between(Some(&at[0]), Some(&at[1]));
        i.add_to_collection(200, list(), song(2), mid);

        assert_eq!(shelved(&i), [song(1), song(2), song(3)]);
        let after = i.catalogue().entries.clone();
        assert!(
            before.is_subset(&after) && after.len() == before.len() + 1,
            "inserting rewrote an existing entry"
        );
    }

    /// Removing one of two identical entries leaves the other, which is why
    /// the position is part of what a removal names.
    #[test]
    fn removing_one_of_two_identical_entries_keeps_the_other() {
        let mut i = instance(b"laptop");
        let keys = appended(&mut i, &[song(1), song(1)]);
        i.remove_from_collection(200, list(), song(1), keys[0].clone());

        assert_eq!(shelved(&i), [song(1)], "both copies went, or neither did");
    }

    /// **A removal is a row that says so**, not an absence.
    ///
    /// The same rule every other retraction follows: an absence cannot tell
    /// "somebody took this out" from "nobody put it in", and a merge with a
    /// device that still has the entry has to be able to.
    #[test]
    fn a_removal_survives_a_merge_with_a_device_that_still_holds_it() {
        let (mut mine, mut theirs) = (instance(b"laptop"), instance(b"nas"));
        let keys = appended(&mut mine, &[song(1), song(2)]);
        theirs.merge(150, mine.records());

        // I take one out; they never hear about it until we sync.
        mine.remove_from_collection(200, list(), song(2), keys[1].clone());
        theirs.merge(300, mine.records());

        assert_eq!(
            Librarian::from_catalogue(&theirs.catalogue()).entries_of(list()),
            [song(1)],
            "the removal did not cross"
        );
    }

    /// **Two devices building the same list agree on it**, and neither's
    /// entries are lost.
    ///
    /// Membership is row existence, so concurrent adds union — and the
    /// positions are what decides the order they union into.
    #[test]
    fn two_devices_adding_at_once_agree_on_the_result() {
        let (mut mine, mut theirs) = (instance(b"laptop"), instance(b"nas"));
        let keys = appended(&mut mine, &[song(1), song(4)]);
        theirs.merge(150, mine.records());

        // Both insert between the two, without seeing each other.
        let a = OrdKey::between(Some(&keys[0]), Some(&keys[1]));
        mine.add_to_collection(200, list(), song(2), a.clone());
        theirs.add_to_collection(200, list(), song(3), a);

        mine.merge(400, theirs.records());
        theirs.merge(400, mine.records());

        let (m, t) = (
            Librarian::from_catalogue(&mine.catalogue()).entries_of(list()),
            Librarian::from_catalogue(&theirs.catalogue()).entries_of(list()),
        );
        assert_eq!(m, t, "the two devices disagree about the list");
        assert_eq!(m.len(), 4, "an entry was lost in the merge");
        assert_eq!(m[0], song(1), "the list did not keep its ends");
        assert_eq!(m[3], song(4));
    }

    /// Moving an entry is a retraction and an assertion, because the position
    /// is part of which entry is meant.
    #[test]
    fn moving_an_entry_is_a_removal_and_an_add() {
        let mut i = instance(b"laptop");
        let keys = appended(&mut i, &[song(1), song(2), song(3)]);

        // Take the last one and put it first.
        i.remove_from_collection(200, list(), song(3), keys[2].clone());
        let front = OrdKey::between(None, Some(&keys[0]));
        i.add_to_collection(200, list(), song(3), front);

        assert_eq!(shelved(&i), [song(3), song(1), song(2)]);
    }

    /// **Two collections do not leak into each other.**
    ///
    /// The entries all live in one set, keyed by collection first — so the
    /// only thing keeping two playlists apart is that key, and every other
    /// test here uses a single list and could not tell.
    #[test]
    fn one_collection_does_not_see_anothers_entries() {
        let other = Id::<Collection>::derived(b"dinner");
        let mut i = instance(b"laptop");
        i.name_collection(100, list(), "Road Trip", true);
        i.name_collection(100, other, "Dinner", true);

        appended(&mut i, &[song(1), song(2)]);
        i.add_to_collection(100, other, song(9), OrdKey::first());

        let l = Librarian::from_catalogue(&i.catalogue());
        assert_eq!(l.entries_of(list()), [song(1), song(2)]);
        assert_eq!(l.entries_of(other), [song(9)]);
        assert_eq!(l.positions_of(other).len(), 1, "positions leaked too");
    }

    /// An unordered collection is the same entity with the flag off — the UI
    /// is free to keep calling the ordered one a playlist.
    #[test]
    fn a_collection_says_whether_its_order_means_anything() {
        let mut i = instance(b"laptop");
        i.name_collection(100, list(), "Odds and ends", false);
        let l = Librarian::from_catalogue(&i.catalogue());
        assert!(!l.collections()[&list()].ordered);
    }
}

#[cfg(test)]
mod perf {
    //! What it costs to rebuild the librarian — issue #123.
    //!
    //! `just bench-fold`. `Stop::Refold` runs this after `:collect` and
    //! `:add-view`, and on a 6,500-row library it was **467 ms** — four fifths
    //! of it inside `draw`, which asked four linear scans of the whole credit
    //! list per row.
    //!
    //! The phases are printed separately because they fail differently, and a
    //! single number would have hidden which one went wrong. Same reason as
    //! `shelf::perf`, and the same lesson: the fold had no measurement, so
    //! nothing said it had gone bad.

    use std::time::Instant;

    use super::*;
    use crate::model::instance::{Instance, Scanned};

    /// A library of `albums` albums, ten tracks each, over `artists` artists —
    /// with credits and genres, because those are what the scans were over.
    fn scan_of(artists: usize, albums: usize) -> Vec<Scanned> {
        const GENRES: [&str; 6] = ["Techno", "House", "Folk", "Rock", "Jazz", "Ambient"];
        let mut scan = Vec::new();
        for a in 0..albums {
            for track in 1..=10u16 {
                let artist = format!("Artist {:04}", a % artists);
                scan.push(Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: (a as u128) * 1000 + u128::from(track),
                    },
                    title: format!("Track {track:02} of {a:04}"),
                    artist: artist.clone(),
                    album: Some(format!("Album {a:04}")),
                    album_artist: Some(artist),
                    genres: vec![GENRES[a % GENRES.len()].to_string()],
                    disc: Some(1),
                    track: Some(track),
                    // **Countable, because the real library is** (#134): 88%
                    // of it carries both totals, and a fixture that carries
                    // neither never runs the counting this measures.
                    track_total: Some(10),
                    disc_total: Some(1),
                    length_ms: 200_000,
                    ..Default::default()
                });
            }
        }
        scan
    }

    /// The table, and the bound that makes it a test rather than a report.
    ///
    /// **Generous on purpose**, like `shelf::perf`. What it has to catch is a
    /// per-row scan reappearing, which costs a factor of ten or more, not a
    /// 20% drift on a busy machine.
    #[test]
    fn rebuilding_the_librarian_does_not_scan_per_row() {
        let device = crate::fixture::device();
        let scan = scan_of(200, 650);

        use crate::accounting::{mb, measure};

        let t = Instant::now();
        let (log, log_kept, log_peak) = measure(|| {
            let mut log = Instance::new(device);
            log.ingest(100, &scan);
            log
        });
        let ingest = t.elapsed();

        let t = Instant::now();
        let (cat, cat_kept, cat_peak) = measure(|| log.catalogue());
        let folded = t.elapsed();

        let t = Instant::now();
        let (mut l, built_kept, built_peak) = measure(|| Librarian::from_catalogue(&cat));
        let built = t.elapsed();
        l.here(device);

        let rows = crate::model::rows::Listing::rows(&l).len();
        // **Bytes beside the times, because #255 asked a question the times cannot
        // answer.** A 7,271-row library sits at ~900 MB across two processes, each
        // holding its own fold, and nothing in the design said what a fold should cost.
        // `kept` is what the stage left behind; `peak` is what it needed on the way, and
        // the gap between them is the transient a refold pays on every rating.
        eprintln!(
            "\n{rows} rows, {} records\n\
             \x20                  time        kept      peak\n  \
             ingest          {ingest:>9.1?}  {:>7.1} MB {:>7.1} MB\n  \
             catalogue()     {folded:>9.1?}  {:>7.1} MB {:>7.1} MB\n  \
             from_catalogue  {built:>9.1?}  {:>7.1} MB {:>7.1} MB\n  \
             {:>32} MB held by all three",
            log.records().len(),
            mb(log_kept),
            mb(log_peak),
            mb(cat_kept),
            mb(cat_peak),
            mb(built_kept),
            mb(built_peak),
            format!("{:.1}", mb(log_kept + cat_kept + built_kept)),
        );

        assert!(rows > 6000, "the bench needs a library to measure");
        assert!(
            built < std::time::Duration::from_millis(400),
            "from_catalogue took {built:?} — something is scanning once per row again"
        );
    }
}
