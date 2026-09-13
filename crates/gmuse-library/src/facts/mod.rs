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

#[cfg(test)]
mod boundary;

use std::collections::{BTreeMap, BTreeSet};

use crate::audio_id::AudioId;
use crate::model::clock::Stamp;
use crate::model::entity::{
    Artist, Collection, Concept, Device, Encoding, Facet, Hierarchy, Label, Medium, Recording,
    Release, ReleaseGroup, Role, Track, View, Work,
};
use crate::model::facet::Described;
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::ord::OrdKey;
use crate::model::reference::{
    AliasKind, ArtistRelationKind, Cataloguable, CreditOn, RoleClass, Source,
};

/// One edge in the artist graph: from whom, to whom, and of which kind.
///
/// The kind is part of the key rather than a value beside it, because a person
/// can be both a member of a band and known by its name — `Bradley Strider`
/// and `Bradley's Beat` — and the two edges answer different questions.
pub type ArtistEdge = (Id<Artist>, Id<Artist>, ArtistRelationKind);

/// What a spectrum measurement found.
///
/// Named rather than a tuple because it grew a third member the moment the
/// second was needed to read the first — `cutoff_hz` says where the audio
/// stops and only `cliff_db` says whether that is an encoder or the music.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spectrum {
    pub cutoff_hz: u32,
    /// Hundredths of a decibel. `None` on a row measured before #184.
    #[serde(default)]
    pub cliff_db: Option<u16>,
    pub analyser: u16,
}

/// When something held: from a date, until one where anybody said.
///
/// Open-ended is the ordinary case, not a gap — a moniker somebody still
/// records under has no end, and inventing one would be worse than the
/// absence.
pub type Period = (PartialDate, Option<PartialDate>);
use crate::model::spine::PartialDate;

/// One thing a device wrote down.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub stamp: Stamp,
    pub fact: Fact,
}

/// The facts this slice needs. The catalogue's full vocabulary arrives with
/// #72–#76; these are the five that the round trip actually exercises.
/// ## Naming
///
/// Every variant is `[Subject][PastTenseVerb]`, which is the event-sourcing
/// convention — `OrderPlaced`, `CustomerRegistered` — and it is followed here
/// without exception.
///
/// Four of these used to be a bare verb: `Measured`, `Rated`, `Played`,
/// `Acquired`. Shorter, and each dropped the one word that mattered.
/// **`Measured` measured what?** An encoding, not a recording — which is the
/// exact distinction this model spends its whole length drawing, thrown away
/// by the name of the fact that carries it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Fact {
    /// An artist's label. **Descriptive** — §4 Type 1, so last writer wins and
    /// there is nothing to surface.
    ArtistNamed { artist: Id<Artist>, name: String },

    /// What a recording's id was derived from.
    ///
    /// **Immutable given the id**, because the id is a function of it — which
    /// is why it is not a field on `RecordingCatalogued`. That fact is
    /// descriptive and last-writer-wins, so a retitle would drag the basis
    /// along with it, and a basis is not the sort of thing a retitle revises.
    ///
    /// Recorded because its absence was the problem: a library where 7.3% of
    /// recordings rest on nothing but their bytes could not say *which* 7.3%,
    /// so the files a re-encode would strand were invisible and the work list
    /// for a fingerprint pass could not be produced.
    RecordingIdentified {
        recording: Id<Recording>,
        basis: crate::model::basis::Basis,
    },

    /// A recording exists, and what it is called.
    RecordingCatalogued {
        recording: Id<Recording>,
        title: String,
        /// **The flattened form, and now a duplication with real notice on
        /// it.** `CreditAsserted` is the statement that survives — it can say
        /// which role, in what order, credited how, and there can be several
        /// of them. This field can say one artist and nothing about them.
        ///
        /// **It has no removal date, and saying so is the correction** (#250). This
        /// read "it stays until #78 dissolves `Track`" — #78 is closed and `Track` is
        /// still here, so the deferral named an owner that had stopped existing. The
        /// tree projection and `Row` still read this field; when they ask for a credit
        /// instead, it goes.
        ///
        /// Until then **the credit is authoritative**: anything that needs to be right
        /// reads `credits`, and this is the fast path for the two callers that predate
        /// it. That part was always the load-bearing half and is unchanged.
        artist: Id<Artist>,
    },

    /// An encoding exists, and what it is. Its id is derived from `audio`, so
    /// two devices reading the same bytes emit the *same* fact.
    ///
    /// **Described, not catalogued.** Cataloguing is the bibliographic act and
    /// it happens one level up, to the `Recording`; DCAT *describes* a
    /// distribution and PREMIS *ingests* an object. Borrowing the word from
    /// the level above suggested this was the same kind of event, and it is
    /// not: nobody makes an editorial decision about a file's bitrate.
    /// **What these bytes say they are, said as a claim** — #160.
    ///
    /// The state the gate needed and did not have. A file no authority names
    /// is refused, and a refusal that leaves no trace means the library has no
    /// record that the bytes exist at all: each import re-refuses them, and
    /// there is nowhere to say "these are being worked on". So the encoding is
    /// described and held, and this records what its tags claimed — while
    /// **no recording is minted**, because nothing has named the performance.
    ///
    /// **A claim is not a value**, which is `info-panel.md`'s rule and the one
    /// this fact exists to keep. `RecordingCatalogued` asserts that a
    /// performance *is* called something; this says only that a tag *said* so.
    /// The two look identical on this material and are not: the album fields
    /// alone yielded a warez stamp as an artist, a chart as a release, and ten
    /// 30-byte ID3v1 stumps.
    ///
    /// It is promoted, not consumed. When an authority finally names the
    /// performance, `Proposal::Identified` catalogues the recording *from this
    /// claim* — the tags were right about the title even when they had no id,
    /// and re-reading the file to learn again what the log already recorded
    /// would be a second door onto the same evidence.
    EncodingClaimed {
        encoding: Id<Encoding>,
        /// Exactly as the tag wrote it, joined credits and all. Nothing here
        /// is normalised, because a claim is evidence and normalising evidence
        /// is editing it.
        artist: String,
        title: String,
    },

    EncodingDescribed {
        encoding: Id<Encoding>,
        audio: AudioId,
        /// `None` when the prober could not say — an unreadable header, not a
        /// missing format.
        format: Option<crate::model::encoding::Format>,
        bitrate: u32,
        /// Playing time in milliseconds. Zero means nobody has said.
        length_ms: u32,
        /// How the audio is shaped — samples per second, and how many
        /// channels.
        ///
        /// Facts about the bytes, read while the file is already open, and
        /// the last two a display row asks for that the model could not
        /// answer: 6,493 of 6,500 files here carry a sample rate and every
        /// one carries a channel count, so a librarian that could not say
        /// them could not replace the scanner as what a row is drawn from
        /// (#86).
        ///
        /// `None` where the prober could not say, which is an unreadable
        /// header rather than a file with no rate.
        sample_rate: Option<u32>,
        channels: Option<u8>,
    },

    /// Which recording an encoding holds. **Structural** — §4 Type 2, so it
    /// carries what it replaced and a disagreement is surfaced.
    EncodingBound {
        encoding: Id<Encoding>,
        from: Option<Id<Recording>>,
        to: Id<Recording>,
    },

    /// A collection exists, and what it is called.
    ///
    /// A playlist and a collection are one entity: a playlist is a collection
    /// whose order means something. `ordered` is the difference and the UI is
    /// free to keep calling an ordered one a playlist.
    CollectionNamed {
        collection: Id<Collection>,
        name: String,
        ordered: bool,
    },

    /// A query somebody named — #119.
    ///
    /// **The source line, not a parsed structure.** It is diffable, it is what
    /// a person typed, and it round-trips through the same parser the `:` line
    /// uses — so an operator added to the language needs no fact added here.
    ///
    /// **In the log rather than the config**, because `data_dir` is where
    /// everything you have decided about your music lives. A named selection
    /// in `config.toml` sits beside the colour scheme: it does not sync, does
    /// not merge, has no provenance, and is invisible to the library
    /// everything else routes through.
    ///
    /// Last-writer-wins on the name, like an artist's: editing a saved view is
    /// a rewrite of one definition, and two devices editing the same one is a
    /// disagreement the clock settles rather than a set to union.
    ViewSaved {
        view: Id<View>,
        name: String,
        source: String,
    },

    /// A saved view somebody deleted. Shadows rather than removes, for the
    /// reason every retraction here does: a stale replica re-delivering the
    /// definition would otherwise resurrect it.
    ViewDropped { view: Id<View> },

    /// What a collection was frozen from — #95.
    ///
    /// **A separate fact, not a field on `CollectionNamed`.** Naming a list
    /// and saying where it came from are two claims, and one of them is
    /// optional: a hand-built crate has no source, and a frozen one has a
    /// query that may be re-run. Adding a field would also have rewritten the
    /// meaning of 6,311 facts already on disk.
    ///
    /// Last-writer-wins on the collection: re-freezing is a rewrite of one
    /// answer, not a set to union.
    ///
    /// This is what makes drift *visible*. Without it, "this crate was
    /// `rating>=4 & genre="*techno*"`, in March" is unanswerable and
    /// re-freezing is a retype rather than a keystroke.
    CollectionFrozen {
        collection: Id<Collection>,
        /// The stack as it was typed — the same round-tripping text a saved
        /// view stores, so one parser reads both.
        source: String,
    },

    /// One recording sits in one collection, at one position.
    ///
    /// **Membership is row existence**, so two devices adding the same track
    /// to the same list union to one entry rather than fighting over a count.
    ///
    /// The position is part of the claim's identity rather than an attribute
    /// of it, which is what lets a **sequence admit duplicates** (#92): the
    /// same recording twice in one playlist is two entries at two positions,
    /// and a set keyed by `(collection, recording)` could not say that. Moving
    /// an entry is therefore a retraction and an assertion, not an update.
    CollectionEntryAdded {
        collection: Id<Collection>,
        recording: Id<Recording>,
        /// A fractional index — see [`crate::model::ord::OrdKey`]. Inserting
        /// between two entries mints a key between theirs and touches no other
        /// row, which is the whole reason a position is not an integer.
        ord: OrdKey,
    },

    /// …and is taken out again.
    ///
    /// Carries the position because that is part of which entry is meant, and
    /// a playlist holding one recording twice has two of them.
    CollectionEntryRemoved {
        collection: Id<Collection>,
        recording: Id<Recording>,
        ord: OrdKey,
    },

    /// An axis of description exists, and what it is called — #117.
    ///
    /// The key is load-bearing twice over: concepts on this axis seed their
    /// ids from it, and the query language names the axis by it. Renaming a
    /// facet's *key* therefore re-mints every concept on it; renaming its
    /// *label* costs nothing, which is why they are two fields.
    FacetDefined {
        facet: Id<Facet>,
        key: String,
        label: String,
    },

    /// A concept exists on one axis, and what it is called.
    ///
    /// Needed before an assertion can name one by id — a device folding a log
    /// has to be able to resolve `Id<Concept>` to something a person reads.
    ///
    /// **Was `GenreCatalogued` and `QualifierCatalogued`** (#117), which were
    /// the same fact written twice. The facet is what told them apart, and it
    /// is a field now.
    ConceptCatalogued {
        facet: Id<Facet>,
        concept: Id<Concept>,
        label: String,
        /// See [`crate::model::facet::ConceptRecord::node_label`].
        node_label: bool,
    },

    /// One grouping of the concepts exists. A vocabulary may carry several —
    /// a browse hierarchy and a filter hierarchy over one list, as in SDMX.
    ///
    /// Scoped to a facet, because an array groups concepts *of one inherent
    /// category* — a hierarchy spanning genre and version would be the two
    /// axes collapsing back into one, which is what #117 exists to stop.
    HierarchyDefined {
        hierarchy: Id<Hierarchy>,
        facet: Id<Facet>,
        name: String,
    },

    /// **These bytes were made out of those bytes** — #132.
    ///
    /// A transcode: the *same performance*, encoded again. Not
    /// [`Fact::RecordingDerived`], which is a musical act — a remix is a
    /// different performance, and this is not.
    ///
    /// **What it is for is disposability.** A derived copy can be made again
    /// from the thing it came from, so it need not be backed up; an acquired
    /// one cannot, whatever its format. Lossless-versus-lossy does not answer
    /// that — a purchased 320 kbps MP3 is a master, and this library holds
    /// lossless files that were upsampled from lossy ones, which is why
    /// `Verdict::Suspect` exists. Only provenance answers it, so it is
    /// recorded rather than inferred from a format or a folder.
    ///
    /// **`from` is an encoding this library holds**, which is the whole
    /// point: a file the seller derived from a master you have never seen is
    /// not regenerable by you, and no edge should claim it is. No edge means
    /// "not known to be regenerable", which is the safe default.
    ///
    /// Nothing records *how*. What came out is already described — its format,
    /// its bitrate, its length — and the recipe belongs to whatever policy
    /// makes derived copies, not to the claim that one was made.
    EncodingDerived {
        from: Id<Encoding>,
        to: Id<Encoding>,
    },
    /// Withdraw one, for the reason every retraction here exists.
    EncodingDerivationRetracted {
        from: Id<Encoding>,
        to: Id<Encoding>,
    },

    /// **One recording is derived from another** — `domain-model.md` §5, #79.
    ///
    /// A remix, an edit, a dub. Row existence, so it unions across devices for
    /// the same reason `narrows` does: there is no `Recording.derivations`
    /// array for two curators to collide over.
    ///
    /// **A cover is not here.** It realises the existing work, as a
    /// performance does, and putting it here would make "which versions of
    /// this recording" answer with things that were never made out of it.
    RecordingDerived {
        /// The original — the thing the new one was made out of.
        from: Id<Recording>,
        to: Id<Recording>,
        kind: crate::model::spine::RecordingDerivationKind,
    },
    /// Withdraw a recording derivation, for the reason every retraction here
    /// exists: a stale replica re-delivering the edge would resurrect it.
    RecordingDerivationRetracted {
        from: Id<Recording>,
        to: Id<Recording>,
        kind: crate::model::spine::RecordingDerivationKind,
    },
    /// **One work is derived from another** — an arrangement, a medley, a
    /// quotation. The other traversal, one level up.
    ///
    /// Separate from the recording relation and it does not reach into it:
    /// recording derivations answer "which versions", work derivations answer
    /// "which arrangements", and a query that crossed between them would be
    /// answering a question nobody asked.
    WorkDerived {
        from: Id<crate::model::entity::Work>,
        to: Id<crate::model::entity::Work>,
        kind: crate::model::spine::WorkDerivationKind,
    },
    WorkDerivationRetracted {
        from: Id<crate::model::entity::Work>,
        to: Id<crate::model::entity::Work>,
        kind: crate::model::spine::WorkDerivationKind,
    },

    /// One narrower-of edge, in one hierarchy.
    ///
    /// Row existence, so it unions across devices and there is no
    /// `Genre.children` for two curators to collide over.
    HierarchyNarrowed {
        hierarchy: Id<Hierarchy>,
        parent: Id<Concept>,
        child: Id<Concept>,
    },

    /// Withdraw a narrower-of edge.
    ///
    /// Without it an edge is permanent, and a wrong one — `Techno` under
    /// `Rock` — cannot be taken back: `genre+ = Rock` keeps returning techno
    /// for good, and the only repair is to stop using the hierarchy.
    ///
    /// Shadows rather than deletes, for the reason every retraction here does:
    /// a stale replica re-delivering the edge would otherwise resurrect it.
    HierarchyNarrowRetracted {
        hierarchy: Id<Hierarchy>,
        parent: Id<Concept>,
        child: Id<Concept>,
    },

    /// **A sourced claim that a concept describes a subject** — #117.
    ///
    /// Reads: *according to `basis`, `subject` is described by `concept`, and
    /// `basis` is this sure.* Not "this recording is techno" — the model never
    /// says that. Truth is a fold over claims and is deliberately never
    /// collapsed, which is why `basis` is part of the key in
    /// [`Catalogue::assertions`] rather than an attribute beside it.
    ///
    /// **Asserted, not assigned.** Neither standard word fits exactly. An
    /// *annotation* (W3C) carries its source natively but takes any body at
    /// all, which is the thing a controlled vocabulary exists to prevent. An
    /// *assignment* (ISO 25964, subject indexing) constrains the body to a
    /// scheme but leaves the indexer implicit and aims at one right answer —
    /// and that suggestion has already cost this codebase once, when
    /// `rows::Listed.genre: Option<&str>` collapsed a set of claims to one
    /// string. What is stored is a claim with a name on it, which is what
    /// `assert_edge`, `CreditAsserted` and `AliasAsserted` already call an
    /// assertion.
    ///
    /// **All four fields**, where the shape before the vocabulary work had two
    /// — `{ recording, genre: String }` — and threw away the subject (only a
    /// recording, where the release group is the right default: genres for 130
    /// of 429 groups against 3 of 49 releases), the concept's identity (a
    /// string cannot reach a hierarchy), and basis and confidence both, so a
    /// Discogs guess and a typed answer became the same fact.
    ConceptAsserted {
        subject: Described,
        concept: Id<Concept>,
        basis: Source,
        confidence: Confidence,
    },

    /// **Everything this device holds, as of now.**
    ///
    /// The whole set, not a diff — which is what a scan produces anyway, and
    /// what lets a device whose holdings drifted be corrected by its next scan
    /// rather than by someone noticing the difference and emitting it.
    ///
    /// This is the one fact that **supersedes** rather than accumulates. Union
    /// would resurrect: delete a file on this machine and a stale replica hands
    /// the row straight back. Only the latest assertion per device is live, and
    /// what makes that safe rather than lossy is that holdings are
    /// single-writer — no other device ever writes this one's.
    HoldingsAsserted {
        device: Id<Device>,
        /// Where each encoding sits on that device. The locator's shape is the
        /// device's business (§2), so it is opaque here.
        held: BTreeMap<Id<Encoding>, String>,
    },

    /// An album exists, and what it is called.
    ReleaseGroupCatalogued {
        group: Id<ReleaseGroup>,
        title: String,
        /// Who the album is credited to, which is not always who a track is.
        artist: Id<Artist>,
        /// When the **album** first appeared, not when this pressing did.
        ///
        /// The reissue trap, and the tags already know: `ORIGINALDATE` says
        /// the album and `DATE` says the pressing, and they disagree for 820
        /// files here. Without it, ordering an artist's albums by date files a
        /// 1996 reissue of a 1995 record after everything from 1995 (#59).
        first_released: Option<PartialDate>,
        /// **Album, single or EP** — #133.
        ///
        /// A field rather than a guess. Every release group used to be an
        /// album because `ReleaseType` had no way in, so a single held
        /// complete and an album held one track of were the same row.
        #[serde(default)]
        kind: crate::model::spine::ReleaseType,
        /// And what else it also is — usually nothing.
        ///
        /// **A set, because the tags carry a set**: real files here say
        /// `album/compilation/dj-mix`. Ordered, so two devices that read the
        /// same tag write the same record and converge without comparing.
        #[serde(default)]
        secondary: std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
    },

    /// Two recordings are one, and this is the name it keeps.
    ///
    /// **The repair for a false split**, and the model's answer to the error
    /// it deliberately prefers: a split is one merge from repaired, a false
    /// merge is permanent. Nothing is deleted — the loser's facts are still in
    /// the log and `undo` puts it back.
    ///
    /// The case that made it necessary is an identification. Naming a file's
    /// performance mints a new recording id, and the byte-minted one it
    /// replaces keeps every rating and play anyone gave it. Applying 377
    /// identifications to this library stranded **7 of 11 ratings and 118 of
    /// 255 played recordings** exactly that way — the facts were all still
    /// there and hung off an id nothing pointed at any more.
    RecordingMerged {
        loser: Id<Recording>,
        winner: Id<Recording>,
    },

    /// **A mastering tool re-encoded this copy, and only the path says so** —
    /// #143.
    ///
    /// The one thing the measurement cannot reach. The cutoff test finds a
    /// lossy ancestor by the wall an encoder leaves; a competent 320-to-320
    /// pass leaves no wall, and 130 of this collection's 144 marked files
    /// measure clean. The `_pn` suffix is the only record that it happened.
    ///
    /// **Read at ingest, because import destroys it.** `shelf render` names a
    /// file `{track} {title}` from the library, so the suffix is gone the
    /// moment the file is placed — measured: 147 marked files on the NAS and
    /// in staging, **0 in the library**. A pass that proposed this afterwards
    /// would have nothing to propose against, which is why it is a scanned
    /// fact and not a proposal.
    ///
    /// **Carries its basis**, because it is evidence like any other claim and
    /// because a tagger may one day say the same thing another way. On the
    /// weakest rung, `Source::Path`: it is recorded and queryable and it never
    /// outranks anything.
    EncodingProcessed {
        encoding: Id<Encoding>,
        basis: crate::model::reference::Source,
    },

    /// **This copy is retired** — the tombstone `storage-layout.md` §6 names
    /// and #159 found missing.
    ///
    /// §6 says how a file leaves: `remove` writes a tombstone, the bytes move
    /// to `.departed/`, and only a separate purge deletes them. **The second
    /// step was built and the first was not.** `.departed` is in the scanner's
    /// skip list with a test asserting *"a walk into .departed hands back
    /// everything `remove` retired"* — carefully built around a first half
    /// that did not exist.
    ///
    /// What that costs: an album retired by moving its folder is still wholly
    /// catalogued, because a *holding* lapses when a scan stops seeing a file
    /// and a *claim* does not. `Early James — Strange Time to Be Alive` drew
    /// 24 rows for a 12-track record.
    ///
    /// **About the encoding, because that is what left.** The recording stays
    /// — §1 is explicit that a row nothing holds is ordinary, and a track you
    /// do not own is a row on purpose (#155). What has to stop is the library
    /// claiming a *copy* it has retired.
    ///
    /// **A shadow, not a deletion.** Nothing in this log is destroyed: a merge
    /// is a redirect and a retraction is a shadow, and this is the same. The
    /// bytes coming back is [`Fact::EncodingRestored`] — one fact, not a
    /// re-import.
    EncodingRetired { encoding: Id<Encoding> },

    /// **A retired copy is back.** The other half, and not optional for the
    /// reason `NarrowingRetracted` is not: a step you cannot take back is one
    /// nobody takes, and §6's whole design is that retiring is reversible and
    /// purging is not.
    EncodingRestored { encoding: Id<Encoding> },

    /// **These two album entries are one album** — #156.
    ///
    /// The same instrument as [`Fact::RecordingMerged`], one level up, and it
    /// exists for the same reason: an id that turned out to name something
    /// already named, with everything that hangs off it still hanging there.
    ///
    /// **A group's id is a function of what the tags said.** It comes from
    /// `mb_release_group` where there is one and from artist-and-title where
    /// there is not, so learning an album's MusicBrainz id *changes its
    /// identity* — and a release, a medium and every slot are minted from the
    /// group in turn. One tag edit therefore mints a whole second album, and
    /// `import` moves the copies to it. Measured on this library, 33 albums
    /// are held as more than one group.
    ///
    /// **Groups only. A merge here says nothing about the pressings under
    /// them**, which is the distinction the case that wanted this turns on:
    /// *Barton Hollow* is a standard edition of 14 tracks and a deluxe with 4
    /// more, and the right answer is **one group holding two releases** — not
    /// one release, and not two albums. Merging downward as well would erase
    /// the difference this model spent a year acquiring the level to hold.
    ///
    /// **And not every duplicate title is one album.** `AC/DC — High Voltage`
    /// is two records, the 1975 Australian and the 1976 international, sharing
    /// a name and a folder. They are correctly two groups, and a rule that
    /// merged on title would have folded them.
    ReleaseGroupMerged {
        loser: Id<ReleaseGroup>,
        winner: Id<ReleaseGroup>,
    },

    /// **This recording is called that** — the recording-level twin of
    /// [`Fact::ArtistNamed`], and the fact an identification could not write.
    ///
    /// **An identification moves the identity, not the description**, and for
    /// the ordinary case that is right: the tags already gave a title and a
    /// MusicBrainz id only settles *which performance* it is, so the label
    /// should not change underneath somebody.
    ///
    /// It is exactly wrong when the title was the thing that was wrong.
    /// Measured: `James Blake — The Bells Sketch` and `Give a Man a Rod` sit
    /// in one folder with their tags **transposed**, and two independent
    /// fingerprints each name the other's title at 0.98, each matching the
    /// other's length. Identifying them moved both bindings correctly and left
    /// both labels wrong, because a relink merges the old recording into the
    /// new one and the old row's title comes along.
    ///
    /// So a tool that has learned a better title needs somewhere to say it.
    /// `identify` has had MusicBrainz's title in hand on every row it has ever
    /// written and no fact to put it in.
    ///
    /// **It outranks a scan, whenever it was written**, and that is not the
    /// rule `ArtistNamed` follows. Last writer wins was tried first and does
    /// not work here: `query` and `apply` bring their view up to date by
    /// walking the disk, and a scan re-asserts `RecordingCatalogued` from the
    /// tags with a fresh stamp — against the *current* recording id, so after
    /// a relink it is a new fact rather than a repeat and survives
    /// `drop_repeats`. Twenty-three retitles applied cleanly and not one was
    /// visible afterwards, because the next read scanned over them.
    ///
    /// So the fold applies these in a pass after the rest. A retitle is
    /// curation and a scan is a reading; curation wins. That is the same
    /// relationship `Source` encodes for a claim, and `RecordingCatalogued`
    /// carries no rung to say it with.
    RecordingRetitled {
        recording: Id<Recording>,
        title: String,
    },

    /// **This album is called that** — the release-group twin of
    /// [`Fact::RecordingRetitled`], and the fact a group merge leaves needing.
    ///
    /// A merge unites what both sides said, and a title is descriptive, so the
    /// last writer takes it. That is how *21* came to be called
    /// `21 (Target Bonus Disc)`: three editions of one record, minted as three
    /// groups because their album *tags* differ, and merging them handed the
    /// name to whichever was written last.
    ///
    /// The same shape has now cost three names — `Various Artists` after an
    /// artist merge, Luna's credit after a group merge, and this. Two of them
    /// had a fact to answer with; this did not.
    ///
    /// **It outranks a scan**, for the reason [`Fact::RecordingRetitled`]
    /// does: `populate` re-asserts `ReleaseGroupCatalogued` from the tags on
    /// every pass, so last-writer-wins would hand the name back to the tag on
    /// the next read, forever. A retitle is curation and a scan is a reading.
    ReleaseGroupRetitled {
        group: Id<ReleaseGroup>,
        title: String,
    },

    /// **These two ids are one artist** — `merge.rs`, and the split
    /// `of_artist` documents as the price of having a name-derived fallback.
    ///
    /// An artist is minted from its MusicBrainz id where the tags carry one
    /// and from its name otherwise, so **one artist arriving both ways mints
    /// two**. Measured on this library: of 1015 named artists, **82 names are
    /// held by more than one id**, and all but one of those pairs is exactly
    /// this — a MusicBrainz-derived id and a name-derived one, spelled the
    /// same after `normalise`.
    ///
    /// Almost none of it is visible. `line_of` dedupes by rendered name, so
    /// two ids that both say `Daft Punk` draw as one; only where the *casing*
    /// also differs — `Umfang` against `UMFANG` — does the split reach a
    /// folder name, which is three of the 82. The other 79 are a real division
    /// of one artist's work that nothing on screen reports.
    ///
    /// **The MusicBrainz id wins**, being the higher rung: it is the one a
    /// second device would also derive, and the name-derived id is the
    /// fallback `of_artist` calls honest rather than good.
    ///
    /// **Two MusicBrainz ids sharing a name are not this.** `Christian
    /// Löffler` is held under two of them here, and MusicBrainz saying two is
    /// evidence *for* two, not a collision to be tidied away. That pair is
    /// left alone.
    ArtistMerged {
        loser: Id<Artist>,
        winner: Id<Artist>,
    },

    /// A composition, as distinct from any performance of it.
    ///
    /// **Nothing in this library's tags produces one.** `MUSICBRAINZ_WORKID`
    /// is a real key and 0 of 6500 files carry it, so the producer here is a
    /// lookup pass through `apply`, never a scan — which is why the fact
    /// exists separately from the ingest ladder rather than beside it.
    WorkCatalogued { work: Id<Work>, title: String },

    /// This recording is a performance of that work.
    ///
    /// Bare, and deliberately so. Which *kind* of performance — instrumental,
    /// live, acappella — is a qualifier on the recording, because 7.3% of this
    /// library has no work at all and a fact hung on the work link could not
    /// mark an instrumental white label. See `model/qualifier.rs`.
    WorkPerformed {
        recording: Id<Recording>,
        work: Id<Work>,
    },

    /// A role exists in the codelist.
    ///
    /// The concept before the credit, exactly as `GenreCatalogued` comes
    /// before `GenreAssigned`: an id no fact resolves is an id nobody can
    /// render.
    RoleDefined {
        role: Id<Role>,
        name: String,
        class: RoleClass,
    },

    /// An artist did something to something, in a role.
    ///
    /// **Row existence**, keyed by subject, artist and role — so an artist who
    /// both performed and produced is two rows, and re-importing either is
    /// idempotent. Position and the credited spelling are attributes on it:
    /// how a sleeve orders and spells a credit is correctable, and correcting
    /// it must not mint a second credit.
    ///
    /// **The basis is in the key**, for the reason it is on an assignment: a
    /// remixer read out of a title is a guess, and MusicBrainz naming the same
    /// remixer is not. Folded to one row the guess and the confirmation are
    /// indistinguishable, and their agreeing — the most useful thing the pair
    /// says — is lost.
    CreditAsserted {
        subject: CreditOn,
        artist: Id<Artist>,
        role: Id<Role>,
        basis: Source,
        position: u8,
        /// What the sleeve printed, when it differs from the artist's name.
        as_credited: Option<String>,
        /// What joins this credit to the next — `" & "`, `" feat. "`.
        join_phrase: Option<String>,
    },

    /// **Somebody has decided this credit is wrong** — and it is the piece
    /// `merge.rs` and #145 both leave needing.
    ///
    /// A joined credit mints one artist nobody is called — `Stephan Bodzin &
    /// Marc Romboy` as a single act — and asserts it. #145 stopped *new* ones
    /// being minted and nothing could withdraw the ones already there: 356
    /// rows in this library are credited to a string naming several people.
    ///
    /// The case that forced it: merging Luna's two release groups united their
    /// credits, so the album was credited both to that joined artist and to
    /// the real one, and `file_under` produced a folder named after both. The
    /// merge was right; there was simply no way to say which of the two
    /// credits it inherited was the mistake.
    ///
    /// **Not a delete, because there are none.** The log is append-only and
    /// merges by union, so a credit is live or not according to whichever of
    /// assert and retract carries the later stamp — the same shape
    /// [`Fact::ConceptRetracted`] takes, for the same reasons.
    ///
    /// **Keyed one field shorter than what it shadows.** A credit is asserted
    /// per `Source`, and somebody saying this artist is not on this record is
    /// answering the question rather than arguing with one asserter. `basis`
    /// records *whose* answer it was, so a later scan cannot quietly undo a
    /// person's correction.
    CreditRetracted {
        subject: CreditOn,
        artist: Id<Artist>,
        role: Id<Role>,
        basis: Source,
    },

    /// **Which slot this copy sits in** — issue #78.
    ///
    /// `TrackPlaced` says a slot exists on a medium and which recording fills
    /// it. This says which *file* is the copy you hold of that slot, and it
    /// exists because a row on screen is a copy rather than a performance.
    ///
    /// Without it, asking a recording which album it is on has several right
    /// answers and no way to choose. A recording appears on every release
    /// that holds it, so `Hells Bells` is on `Back in Black` and on the
    /// compilation `Who Made Who`; the file in the `Back in Black` folder is
    /// on one of them and the model could not say which. Taking the first
    /// placement moved tracks between 60 albums on this library (#84).
    EncodingPlaced {
        encoding: Id<Encoding>,
        track: Id<Track>,
        /// **Who says this copy sits here** — #170.
        ///
        /// The fold used to rank two placements by a rung it could *compute*:
        /// a release minted from its group is weaker than one minted from a
        /// MusicBrainz id, and no fact needed a field to say which. That held
        /// while the only author was a scan.
        ///
        /// It stops holding the moment two placements point at MusicBrainz
        /// releases and disagree about which one. Adele's *21* is one folder
        /// of eighteen files whose tags name three pressings; MusicBrainz has
        /// a single eighteen-track edition that seats all of them. Both are
        /// id-derived, so the computed rung cannot separate them and recency
        /// decides — and a scan is always the most recent thing that ran.
        ///
        /// So it is recorded. `TagVariant` is what a scan can say; a pass that
        /// read the whole tracklist and matched it against every file held
        /// says more than that, and outranks it.
        ///
        /// Defaulted for the records written before this existed, which were
        /// all scans.
        #[serde(default = "Source::of_a_tag")]
        basis: Source,
    },

    /// **ReplayGain, as the tags claim it.**
    ///
    /// A sibling of `EncodingDescribed` rather than a field on it, for the
    /// reason `EncodingMeasured` is separate: that one says what the bytes
    /// *are*, and this is what a tagger *said about* them. The two move
    /// independently — writing ReplayGain across this library changed every
    /// file's mtime in an afternoon and not one sample of audio.
    ///
    /// Every one of the 6,500 files here carries both a track and an album
    /// value, so a librarian that could not say them could not hand a fader
    /// what to apply (#86).
    EncodingLevelled {
        encoding: Id<Encoding>,
        levels: crate::model::encoding::Levels,
    },

    /// An imprint exists, and what it is called.
    ///
    /// The concept before the reference, as `GenreCatalogued` comes before
    /// `GenreAssigned`: an id no fact resolves is an id nobody can render.
    ///
    /// Its own kind rather than a string on the release, because a label is a
    /// thing the library holds an opinion about — a sampler's `album_artist`
    /// is "Various Artists", so the imprint has nowhere else to go, and
    /// Bandcamp writes it into `album_artist` for want of anywhere better,
    /// which is what makes a label show up as though it were an artist.
    LabelNamed { label: Id<Label>, name: String },

    /// One pressing of an album.
    ///
    /// The level between the group and the disc, and the one this model spent
    /// its first year without: "the 2015 reissue has a bonus disc" is a claim
    /// about a *release*, and with only a group to hang it on there was
    /// nowhere to put it. The group is the work-shaped thing — *Kid A* — and
    /// the release is the object you own.
    /// **What a release's id was derived from** — #204.
    ///
    /// Its own fact rather than a field on [`Fact::ReleaseCatalogued`], for
    /// the reason [`Fact::RecordingIdentified`] gives for recordings and which
    /// applies here word for word: **immutable given the id**, because the id
    /// is a function of it. `ReleaseCatalogued` is descriptive and taken
    /// wholesale — restating a pressing clears its label, its date and the
    /// disc count #134 put there — so a retitle would drag the basis along
    /// with it, and a basis is not the sort of thing a retitle revises.
    ///
    /// #203 put it on `ReleaseCatalogued` and that was wrong on both counts:
    /// a retitle would have carried it, and a pass that knew only the mbid
    /// could not record one without restating everything else.
    ///
    /// **Why it has to be recorded at all.** `Id::<Release>::of_musicbrainz`
    /// is a UUIDv5 over the mbid — convergent, so two machines mint the same
    /// id without speaking, and one-way, so the mbid is not recoverable. The
    /// derivation settles *identity* and says nothing about *lookup*, and a
    /// pass that wants to ask MusicBrainz about a pressing needs the second.
    ///
    /// **Row existence, never superseded.** Two devices identifying one
    /// pressing union; nothing here can disagree, because an id derives from
    /// exactly one mbid.
    ReleaseIdentified {
        release: Id<Release>,
        musicbrainz: uuid::Uuid,
    },

    ReleaseCatalogued {
        release: Id<Release>,
        group: Id<ReleaseGroup>,
        title: String,
        /// When this pressing came out, which is **not** when the album did.
        /// #59's reissue trap is exactly this distinction: a 2015 pressing of
        /// a 1982 album sorts by 1982, and only the group knows that.
        released: Option<PartialDate>,
        /// Who issued it. **On the pressing, not on the album**, because that
        /// is what a reissue changes: the record is the same and the imprint
        /// is not.
        label: Option<Id<Label>>,
        /// **How many discs this pressing has** — #134.
        ///
        /// Without it, a release whose second disc has never been seen has no
        /// medium row at all, and "do I hold all of it" answers yes. That is
        /// the one wrong answer this field exists to prevent.
        #[serde(default)]
        media: Option<u16>,
    },

    /// A disc or a side of a release.
    MediumCatalogued {
        medium: Id<Medium>,
        release: Id<Release>,
        /// Which disc, as the tags claim.
        position: u16,
        /// `CD`, `12" Vinyl`, `Digital Media`. Nothing reads it yet; it is
        /// here because a medium without one is a disc nobody can describe,
        /// and adding the field later means a migration.
        format: Option<String>,
        /// **How many tracks this disc has** — #134, and the denominator the
        /// library never had. `None` is "nobody said", which must stay
        /// distinct from "I have them all": 12% of this library carries no
        /// total at all.
        #[serde(default)]
        tracks: Option<u16>,
    },

    /// Where a recording sits **on a medium**.
    ///
    /// The position is what the **tags claim**, not where the row happens to
    /// fall. A claim can be checked against its neighbours; a position derived
    /// from ordering cannot disagree with itself and so can never say the
    /// source was wrong.
    ///
    /// It used to name a release *group* and carry a disc number, which
    /// flattened two levels into one: a recording does not sit on an album, it
    /// sits on a disc of a pressing of one, and the disc number was standing
    /// in for the medium that was missing.
    TrackPlaced {
        track: Id<Track>,
        medium: Id<Medium>,
        recording: Id<Recording>,
        position: u16,
        /// What the sleeve printed here, when it differs from the recording's
        /// own title. A compilation renaming a track is a fact about the
        /// compilation, and belongs on the slot rather than on the song.
        title_on_release: Option<String>,
    },

    /// What an analyser measured about an encoding.
    ///
    /// A **separate pass**, not part of a scan. An FFT over every file is
    /// minutes rather than the seconds hashing costs, and the answer does not
    /// change until either the bytes or the analyser do — so it is recorded
    /// once and carries the analyser that produced it, which is what lets a
    /// re-measure target stale rows instead of the library.
    ///
    /// This is what makes a verdict possible, and without it every encoding is
    /// `Unmeasured` and "is this a transcode" cannot be asked at all.
    EncodingMeasured {
        encoding: Id<Encoding>,
        /// Where the spectrum stops, in Hz.
        cutoff_hz: u32,
        /// **How far it falls in the kilohertz above, in hundredths of a
        /// decibel — and without it the cutoff cannot be read** (#184).
        ///
        /// An encoder's lowpass drops vertically; music rolls off. The cutoff
        /// says *where* the spectrum ends and only this says *why*, so a rule
        /// reading the cutoff alone calls a quiet recording a transcode:
        ///
        /// ```text
        /// Interference  FLAC   8.1 kHz  10 dB   music, nothing up there
        /// People…       MP3   20.0 kHz  56 dB   an encoder's wall
        /// ```
        ///
        /// It ranks the first as the worse file. It is the better one.
        ///
        /// **`None` for the 7,210 written before this**, which is honest
        /// rather than a gap to paper over: they were taken by a pass that did
        /// not record it, and pretending to a value would be worse than
        /// admitting the measurement is half. `analyser` is what a re-measure
        /// already targets, so filling them in is the route that exists.
        #[serde(default)]
        cliff_db: Option<u16>,
        analyser: u16,
    },

    /// Another name for something, so a name that was once right stays
    /// findable.
    ///
    /// **Row existence, and never superseded.** An alias is additive by
    /// nature: two devices each recording one union to both, and a name the
    /// world used does not stop having been used. Nothing here has a
    /// last-writer rule because nothing here can disagree.
    AliasAsserted {
        subject: Cataloguable,
        value: String,
        kind: AliasKind,
    },

    /// A name this subject was authoritatively called, and is not called now.
    ///
    /// **A different class from an alias, not a kind of one** (#176). MADS/RDF
    /// keeps `Variant` and `DeprecatedAuthority` disjoint because a deprecated
    /// heading was the authorised one once and a variant never was, and the
    /// distinction earns its keep here twice over: a merge retires the loser's
    /// label, and a concept rename retires the term it was found under. Both
    /// are names *this library itself printed*, so they travel in old tags and
    /// old exports, and reading one back has to recognise it rather than mint
    /// a second row. A stage name never had that problem.
    ///
    /// **It is an alias as well**, and the fold records both: this says what
    /// happened to the name, `aliases` keeps it findable. Row existence with
    /// no last-writer rule, the same as [`Fact::AliasAsserted`] — two devices
    /// retiring one heading union, and a name that stopped being right does
    /// not start again.
    HeadingRetired {
        subject: Cataloguable,
        value: String,
    },

    /// One artist to another: a moniker, or a membership — #174.
    ///
    /// **Two rows kept, not one made** — which is the whole of the difference
    /// from [`Fact::ArtistMerged`]. Richard D. James releases as Aphex Twin,
    /// AFX, Polygon Window and Caustic Window, and both halves are true at
    /// once: four folders is the right default, because a release is credited
    /// to a moniker and `{file_under}` is that credit; and one shelf is the
    /// right question sometimes. Merging AFX into Aphex Twin would answer the
    /// second by destroying the first.
    ///
    /// **Not an alias label either.** [`Fact::AliasAsserted`] says *this
    /// string is another name for this artist* — one row, several spellings.
    /// This says *these two rows are one act*, and each keeps its own
    /// credits, plays and folder.
    ///
    /// **Directional, and the direction is the answer to "which is
    /// primary".** `from` is the moniker and `to` the act behind it. Nothing
    /// computes that — see [`ArtistRelationKind::AliasOf`] — it is declared,
    /// the way MusicBrainz declares sixteen `is person` edges rather than
    /// deriving a root.
    ///
    /// Row existence with no last-writer rule, like the aliases above: two
    /// devices recording one relation union, and a relation somebody stated
    /// does not stop having been stated.
    ArtistRelated {
        from: Id<Artist>,
        to: Id<Artist>,
        kind: ArtistRelationKind,
        period: Option<Period>,
    },
    /// Somebody has decided a concept does not apply.
    ///
    /// **Not a delete, because there are none.** The log is append-only and
    /// merges by union, so removing an imported claim would destroy the
    /// provenance saying where it came from, and a stale replica would deliver
    /// it again on the next sync. This shadows instead: the pair is live or
    /// not according to whichever of assert and retract carries the later
    /// stamp, which the hybrid clock already makes total.
    ///
    /// Deliberately *not* "assertions outnumber retractions" — that counting
    /// rule turns a disagreement into a war nobody can end.
    ///
    /// **Keyed one field shorter than what it shadows**, and see
    /// [`Catalogue::assertions_retracted`] for why: somebody saying "this is
    /// not live" is answering the question, not arguing with one asserter.
    ConceptRetracted {
        subject: Described,
        concept: Id<Concept>,
        /// **Who ruled it out**, so precedence can be applied (#96).
        ///
        /// A retraction shadows every basis at once — somebody saying "this is
        /// not live" is answering the question, not arguing with one source —
        /// but *whose* answer it was still matters, because the next import
        /// must not quietly undo a person's correction.
        basis: Source,
    },

    /// A rating, or its removal with `None`.
    ///
    /// **The latest by causal order wins**, and none is discarded — a rating
    /// set on two devices during one disconnected window resolves to a single
    /// value while both stay in the log.
    RecordingRated {
        recording: Id<Recording>,
        stars: Option<u8>,
    },

    /// A play. Counted, never a stored number.
    RecordingPlayed {
        recording: Id<Recording>,
        encoding: Option<Id<Encoding>>,
    },

    /// When this entered the library.
    ///
    /// **Folded as a minimum, which makes it the one fact needing no rule.**
    /// Min is commutative, associative and idempotent, so two devices that
    /// each acquired a recording converge on the earlier date without a clock
    /// comparison, a last-writer rule, or anything to surface. Where §4 has to
    /// decide between two writes, this one does not have two.
    ///
    /// It replaces `history.toml`'s `added`, and is a better fact than it was:
    /// "when the library got this" travels between machines, where "when this
    /// device first saw the file" was true only here and reset whenever a file
    /// was re-imported. Deliberately **not** the file's mtime, which moves for
    /// reasons that have nothing to do with the music — writing ReplayGain
    /// across the library reset every one of them in an afternoon.
    RecordingAcquired {
        recording: Id<Recording>,
        /// Seconds since the epoch. Carried rather than taken from the stamp,
        /// so a migration can say when something was *actually* acquired
        /// instead of when it was converted.
        at: i64,
    },
}

/// **How much of a projection one fact can change** — #192.
///
/// `advance()` has to apply a *single* fact to a projection that already
/// exists. If the only move is `rebuild`, a keystroke in the TUI costs a replay
/// of 279,349 records and the design of `access-layers.md` collapses back to
/// what it replaced.
///
/// So every fact declares its blast radius, and the declaration is an
/// exhaustive `match` rather than a list somebody maintains: a new variant does
/// not compile until it has one.
///
/// **When it is not obvious, the answer is [`Blast::Wide`].** `Narrow` is the
/// optimisation and `Wide` is the safe one — a fact classified narrower than it
/// is leaves the projection *wrong*, which is a different kind of bad from
/// leaving it slow. Nothing here should ever be guessed downward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; classified now so the projector cannot be built without it — #193 is the caller"
)]
pub enum Blast {
    /// **The fact names what it affects**, so applying it is a lookup.
    ///
    /// Size is not the question. A fact naming seven thousand encodings is
    /// narrow if it names them; a fact naming two ids is wide if the rows it
    /// reaches have to be searched for.
    Narrow,
    /// **The affected set is not derivable from the fact**, so applying it
    /// means searching the projection for what it hit.
    ///
    /// `ArtistMerged` hands over two ids and leaves "every row crediting the
    /// loser" to be found. That search is what costs — **not** the size of the
    /// fact: `HoldingsAsserted` is the largest fact this log holds and is
    /// narrow, because it names every encoding it touches.
    ///
    /// Still incremental: a query over the projection, never a replay of the
    /// log.
    Wide,
    /// Cannot be localised at all: replay.
    ///
    /// **This should stay empty**, and it is. A fact that quietly needs a
    /// rebuild is a stutter nobody can predict; one that is *documented* to
    /// need it is a cost a person can be told about.
    Rebuild,
}

/// One entry a fact reaches.
///
/// Wider than [`Cataloguable`], which is the eight kinds a heading can hang
/// off: a fact also touches encodings, collections, media and the rest, and a
/// change notification has to be able to name any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Touched {
    Artist(Id<Artist>),
    Recording(Id<Recording>),
    Encoding(Id<Encoding>),
    Collection(Id<Collection>),
    View(Id<crate::model::entity::View>),
    Facet(Id<Facet>),
    Concept(Id<Concept>),
    Hierarchy(Id<Hierarchy>),
    Work(Id<Work>),
    Device(Id<Device>),
    ReleaseGroup(Id<ReleaseGroup>),
    Release(Id<Release>),
    Medium(Id<Medium>),
    Track(Id<Track>),
    Role(Id<Role>),
    Label(Id<Label>),
}

impl From<Cataloguable> for Touched {
    fn from(c: Cataloguable) -> Self {
        match c {
            Cataloguable::Work(id) => Touched::Work(id),
            Cataloguable::Recording(id) => Touched::Recording(id),
            Cataloguable::Release(id) => Touched::Release(id),
            Cataloguable::ReleaseGroup(id) => Touched::ReleaseGroup(id),
            Cataloguable::Artist(id) => Touched::Artist(id),
            Cataloguable::Concept(id) => Touched::Concept(id),
            Cataloguable::Role(id) => Touched::Role(id),
            Cataloguable::Label(id) => Touched::Label(id),
        }
    }
}

impl From<Described> for Touched {
    fn from(d: Described) -> Self {
        match d {
            Described::ReleaseGroup(id) => Touched::ReleaseGroup(id),
            Described::Recording(id) => Touched::Recording(id),
        }
    }
}

impl From<CreditOn> for Touched {
    fn from(c: CreditOn) -> Self {
        match c {
            CreditOn::Work(id) => Touched::Work(id),
            CreditOn::Recording(id) => Touched::Recording(id),
            CreditOn::ReleaseGroup(id) => Touched::ReleaseGroup(id),
            CreditOn::Release(id) => Touched::Release(id),
        }
    }
}

impl Fact {
    /// Which entries folding this fact can reach, **named**.
    ///
    /// Only meaningful for [`Blast::Narrow`], and that is the definition of
    /// narrow rather than a caveat: a wide fact is wide precisely because the
    /// set it affects is not derivable from it, so what this returns for one
    /// is the ids it *mentions* and not the ids it changes. A notification
    /// built from it must say `Wide` and let the reader re-query.
    ///
    /// Exhaustive on purpose, like [`Fact::blast`]. A new fact does not
    /// compile until somebody says what it reaches, which is the only thing
    /// that stops this drifting quietly out of step with the fold.
    pub fn touches(&self) -> Vec<Touched> {
        match self {
            Fact::ArtistNamed { artist, .. } | Fact::ArtistMerged { winner: artist, .. } => {
                vec![Touched::Artist(*artist)]
            }
            Fact::ArtistRelated { from, to, .. } => {
                vec![Touched::Artist(*from), Touched::Artist(*to)]
            }
            Fact::RecordingIdentified { recording, .. }
            | Fact::RecordingCatalogued { recording, .. }
            | Fact::RecordingRetitled { recording, .. }
            | Fact::RecordingRated { recording, .. }
            | Fact::RecordingPlayed { recording, .. }
            | Fact::RecordingAcquired { recording, .. } => vec![Touched::Recording(*recording)],
            Fact::RecordingMerged { loser, winner } => {
                vec![Touched::Recording(*loser), Touched::Recording(*winner)]
            }
            Fact::RecordingDerived { from, to, .. }
            | Fact::RecordingDerivationRetracted { from, to, .. } => {
                vec![Touched::Recording(*from), Touched::Recording(*to)]
            }
            Fact::WorkPerformed { recording, work } => {
                vec![Touched::Recording(*recording), Touched::Work(*work)]
            }
            Fact::WorkCatalogued { work, .. } => vec![Touched::Work(*work)],
            Fact::WorkDerived { from, to, .. } | Fact::WorkDerivationRetracted { from, to, .. } => {
                vec![Touched::Work(*from), Touched::Work(*to)]
            }
            Fact::EncodingClaimed { encoding, .. }
            | Fact::EncodingDescribed { encoding, .. }
            | Fact::EncodingProcessed { encoding, .. }
            | Fact::EncodingRetired { encoding }
            | Fact::EncodingRestored { encoding }
            | Fact::EncodingLevelled { encoding, .. }
            | Fact::EncodingMeasured { encoding, .. } => vec![Touched::Encoding(*encoding)],
            // **The recordings too**, because binding moves the encoding from
            // one to the other and both rows change.
            Fact::EncodingBound { encoding, from, to } => {
                let mut out = vec![Touched::Encoding(*encoding), Touched::Recording(*to)];
                out.extend(from.map(Touched::Recording));
                out
            }
            Fact::EncodingPlaced {
                encoding, track, ..
            } => {
                vec![Touched::Encoding(*encoding), Touched::Track(*track)]
            }
            Fact::EncodingDerived { from, to } | Fact::EncodingDerivationRetracted { from, to } => {
                vec![Touched::Encoding(*from), Touched::Encoding(*to)]
            }
            Fact::CollectionNamed { collection, .. }
            | Fact::CollectionFrozen { collection, .. } => vec![Touched::Collection(*collection)],
            Fact::CollectionEntryAdded {
                collection,
                recording,
                ..
            }
            | Fact::CollectionEntryRemoved {
                collection,
                recording,
                ..
            } => vec![
                Touched::Collection(*collection),
                Touched::Recording(*recording),
            ],
            Fact::ViewSaved { view, .. } | Fact::ViewDropped { view } => vec![Touched::View(*view)],
            Fact::FacetDefined { facet, .. } => vec![Touched::Facet(*facet)],
            Fact::ConceptCatalogued { facet, concept, .. } => {
                vec![Touched::Facet(*facet), Touched::Concept(*concept)]
            }
            Fact::HierarchyDefined { hierarchy, .. } => vec![Touched::Hierarchy(*hierarchy)],
            Fact::HierarchyNarrowed {
                hierarchy,
                parent,
                child,
            }
            | Fact::HierarchyNarrowRetracted {
                hierarchy,
                parent,
                child,
            } => vec![
                Touched::Hierarchy(*hierarchy),
                Touched::Concept(*parent),
                Touched::Concept(*child),
            ],
            Fact::ConceptAsserted { subject, .. } | Fact::ConceptRetracted { subject, .. } => {
                vec![Touched::from(*subject)]
            }
            Fact::CreditAsserted {
                subject,
                artist,
                role,
                ..
            }
            | Fact::CreditRetracted {
                subject,
                artist,
                role,
                ..
            } => vec![
                Touched::from(*subject),
                Touched::Artist(*artist),
                Touched::Role(*role),
            ],
            Fact::AliasAsserted { subject, .. } | Fact::HeadingRetired { subject, .. } => {
                vec![Touched::from(*subject)]
            }
            // **Every encoding it names**, which is the whole of why the
            // largest fact in the log is narrow.
            Fact::HoldingsAsserted { device, held } => {
                let mut out = vec![Touched::Device(*device)];
                out.extend(held.keys().copied().map(Touched::Encoding));
                out
            }
            Fact::ReleaseGroupCatalogued { group, artist, .. } => {
                vec![Touched::ReleaseGroup(*group), Touched::Artist(*artist)]
            }
            Fact::ReleaseGroupRetitled { group, .. } => vec![Touched::ReleaseGroup(*group)],
            Fact::ReleaseGroupMerged { loser, winner } => vec![
                Touched::ReleaseGroup(*loser),
                Touched::ReleaseGroup(*winner),
            ],
            Fact::ReleaseIdentified { release, .. } => vec![Touched::Release(*release)],
            Fact::ReleaseCatalogued { release, group, .. } => {
                vec![Touched::Release(*release), Touched::ReleaseGroup(*group)]
            }
            Fact::MediumCatalogued {
                medium, release, ..
            } => {
                vec![Touched::Medium(*medium), Touched::Release(*release)]
            }
            Fact::TrackPlaced {
                track,
                medium,
                recording,
                ..
            } => vec![
                Touched::Track(*track),
                Touched::Medium(*medium),
                Touched::Recording(*recording),
            ],
            Fact::RoleDefined { role, .. } => vec![Touched::Role(*role)],
            Fact::LabelNamed { label, .. } => vec![Touched::Label(*label)],
        }
    }
}

impl Fact {
    /// **Whether a view may propose this over the socket** — #236, §8.4, I15.
    ///
    /// `LibraryOp::Edit` is "the facts a view emits directly", and this is that
    /// sentence made checkable. An attached view holds a reading desk and cannot
    /// append (#219), so its writes travel as proposed facts and the engine — the
    /// process's single appender, per §4.4 — decides whether to take them.
    ///
    /// **The list is what the UI can actually produce, and nothing more.** Every
    /// other fact comes from somewhere a view is not: an import reads a file, a
    /// scan walks a disk, an identification pass asks MusicBrainz, the analyser
    /// measures bytes. A view proposing `EncodingMeasured` would be claiming a
    /// measurement it never took, and `HoldingsAsserted` would be one machine
    /// telling the log what another machine holds.
    ///
    /// Widening it is a deliberate act, which is what
    /// `every_fact_says_whether_a_view_may_propose_it` is for: adding a variant
    /// to the enum forces a decision here rather than inheriting a default.
    pub fn is_an_edit(&self) -> bool {
        match self {
            // A person rating a song, and the play that a listen earned. The
            // two things the UI writes constantly.
            Fact::RecordingRated { .. } | Fact::RecordingPlayed { .. } => true,

            // Crates: naming one, its membership, and freezing a question into
            // an answer somebody kept.
            Fact::CollectionNamed { .. }
            | Fact::CollectionEntryAdded { .. }
            | Fact::CollectionEntryRemoved { .. }
            | Fact::CollectionFrozen { .. } => true,

            // Saved views, and dropping one. `ViewDropped` is not emitted by
            // the TUI today and is here because it is the exact inverse of one
            // that is — refusing it would make a view removable only by the
            // process that could save it.
            Fact::ViewSaved { .. } | Fact::ViewDropped { .. } => true,

            // `:assert` writes these — a genre, a mood, an era said by a
            // person, which is the `Source::Manual` that outranks an import.
            // The retraction for the same reason `ViewDropped` is here.
            //
            // **`ConceptCatalogued` comes with them and is not curation.**
            // `Instance::assert_concept` emits *two* facts: the term, minted
            // deterministically from `(facet, label)`, and the assertion about the
            // subject. Refusing the first — which an earlier cut of this did, on
            // the reasoning that the vocabulary is curated — made an attached
            // `:assert` fail as a whole batch, because a term nobody had used
            // before does not exist yet. What `genre-hierarchy-is-curated` is about
            // is the *shape*: where a term sits, which is `HierarchyDefined` and
            // `HierarchyNarrowed` below, and `node_label: false` here says this
            // claims no position.
            Fact::ConceptAsserted { .. }
            | Fact::ConceptRetracted { .. }
            | Fact::ConceptCatalogued { .. } => true,

            // **Everything below is somebody else's to say**, and the grouping
            // is deliberate rather than a catch-all: each line names where the
            // fact actually comes from, so a new variant landing in the wrong
            // group is visible.

            // An import: what a file claims about itself, read once, when
            // somebody declares it belongs here.
            Fact::ArtistNamed { .. }
            | Fact::RecordingIdentified { .. }
            | Fact::RecordingCatalogued { .. }
            | Fact::EncodingClaimed { .. }
            | Fact::EncodingDescribed { .. }
            | Fact::EncodingBound { .. }
            | Fact::ReleaseGroupCatalogued { .. }
            | Fact::WorkCatalogued { .. }
            | Fact::WorkPerformed { .. }
            | Fact::RoleDefined { .. }
            | Fact::CreditAsserted { .. }
            | Fact::CreditRetracted { .. }
            | Fact::LabelNamed { .. }
            | Fact::ReleaseIdentified { .. }
            | Fact::ReleaseCatalogued { .. }
            | Fact::MediumCatalogued { .. }
            | Fact::TrackPlaced { .. }
            | Fact::AliasAsserted { .. }
            | Fact::ArtistRelated { .. } => false,

            // A scan, a placement, or the analyser: what this machine found on
            // its own disk, and what the bytes measure.
            Fact::HoldingsAsserted { .. }
            | Fact::EncodingPlaced { .. }
            | Fact::EncodingLevelled { .. }
            | Fact::EncodingMeasured { .. }
            | Fact::EncodingProcessed { .. }
            | Fact::EncodingRetired { .. }
            | Fact::EncodingRestored { .. }
            | Fact::RecordingAcquired { .. } => false,

            // The *shape* of the vocabulary, which `tools/` curates as a whole
            // rather than a row at a time (`genre-hierarchy-is-curated`) — and the
            // facets, which are the axes themselves and not terms on one.
            Fact::FacetDefined { .. }
            | Fact::HierarchyDefined { .. }
            | Fact::HierarchyNarrowed { .. }
            | Fact::HierarchyNarrowRetracted { .. } => false,

            // Derivations: a relation one pass computed from another.
            Fact::EncodingDerived { .. }
            | Fact::EncodingDerivationRetracted { .. }
            | Fact::RecordingDerived { .. }
            | Fact::RecordingDerivationRetracted { .. }
            | Fact::WorkDerived { .. }
            | Fact::WorkDerivationRetracted { .. } => false,

            // **Merges and retitles, and this is the one that is arguable.**
            // §8.4's own summary of `edit` says "collections, ratings, merges",
            // and the UI has no gesture that emits one — `shelf` does, having
            // looked at `shelf duplicates`, which the project is explicit is a
            // brief and not a work list. A wrong merge is not one merge from
            // repaired, so it stays off the wire until a view has a gesture
            // that earns it.
            Fact::RecordingMerged { .. }
            | Fact::ReleaseGroupMerged { .. }
            | Fact::ArtistMerged { .. }
            | Fact::RecordingRetitled { .. }
            | Fact::ReleaseGroupRetitled { .. }
            | Fact::HeadingRetired { .. } => false,
        }
    }

    /// What folding this fact can reach.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, and #193's `advance` is what will read it"
    )]
    pub fn blast(&self) -> Blast {
        match self {
            // ---- narrow: the fact names its subject and that is the extent --

            // One recording, one encoding, one slot, one rating. The ordinary
            // case, and the reason `every_fact_names_its_subject` is a useful
            // neighbour: **a fact that names its subject is a fact whose blast
            // radius can be computed.**
            Fact::RecordingIdentified { .. }
            | Fact::RecordingCatalogued { .. }
            | Fact::RecordingRetitled { .. }
            | Fact::RecordingRated { .. }
            | Fact::RecordingPlayed { .. }
            | Fact::RecordingAcquired { .. }
            | Fact::EncodingClaimed { .. }
            | Fact::EncodingDescribed { .. }
            | Fact::EncodingProcessed { .. }
            | Fact::EncodingLevelled { .. }
            | Fact::EncodingMeasured { .. }
            | Fact::EncodingPlaced { .. }
            | Fact::EncodingRetired { .. }
            | Fact::EncodingRestored { .. }
            | Fact::TrackPlaced { .. }
            | Fact::WorkPerformed { .. } => Blast::Narrow,

            // **Two recordings, not one** — the encoding leaves `from` and
            // joins `to`, so both sides are touched. Still bounded by the fact.
            Fact::EncodingBound { .. } => Blast::Narrow,

            // Pairs, each naming both ends.
            Fact::EncodingDerived { .. }
            | Fact::EncodingDerivationRetracted { .. }
            | Fact::RecordingDerived { .. }
            | Fact::RecordingDerivationRetracted { .. }
            | Fact::WorkDerived { .. }
            | Fact::WorkDerivationRetracted { .. } => Blast::Narrow,

            // Catalogue entries for things no row has yet. A release group
            // gains rows only when a track is placed on it, which is its own
            // fact.
            Fact::WorkCatalogued { .. }
            // One entry, and the one the fact names. It is additive and never
            // superseded — an id derives from exactly one mbid — so it cannot
            // reach past the pressing it identifies.
            | Fact::ReleaseIdentified { .. }
            | Fact::ReleaseGroupCatalogued { .. }
            | Fact::ReleaseCatalogued { .. }
            | Fact::MediumCatalogued { .. } => Blast::Narrow,

            // **Narrow, and this pair is why the tiers had to be re-read.**
            //
            // The fold writes `cat.artist_names.insert(who(artist), name)` —
            // one entry. It looked wide because the *drawn rows* carry the
            // name, and drawn rows are not the projection: they are L2's
            // materialisation, rebuilt per fold and never stored, so no fact
            // has to reach in and correct one.
            //
            // Classified against `Drawn` these were wide. Classified against
            // the `Catalogue`, which is what L1 actually is, they are a map
            // insert. The tier follows the projection, and the projection had
            // been mistaken for the view of it.
            Fact::ArtistNamed { .. } | Fact::LabelNamed { .. } => Blast::Narrow,

            // Vocabulary. Defining a facet, a concept, a role or a hierarchy
            // adds a term; it does not assert anything about a row.
            Fact::FacetDefined { .. }
            | Fact::ConceptCatalogued { .. }
            | Fact::HierarchyDefined { .. }
            | Fact::RoleDefined { .. } => Blast::Narrow,

            // Collections and views. A collection entry names one recording;
            // a view is not a row at all.
            Fact::CollectionNamed { .. }
            | Fact::CollectionFrozen { .. }
            | Fact::CollectionEntryAdded { .. }
            | Fact::CollectionEntryRemoved { .. }
            | Fact::ViewSaved { .. }
            | Fact::ViewDropped { .. } => Blast::Narrow,

            // One subject's assertion on one axis.
            Fact::ConceptAsserted { .. } | Fact::ConceptRetracted { .. } => Blast::Narrow,

            // ---- wide: bounded by what it hit, not by what it says --------

            // **The merges, and now the only wide facts there are.**
            //
            // Every other tier fell to narrow once the projection was read as
            // the `Catalogue` rather than as the rows drawn from it. These do
            // not, and the fold shows why: it applies merges in a **first
            // pass** and then resolves every id it meets through the finished
            // set, because a merge recorded on Tuesday has to move a rating
            // given on Monday. Applying one to a projection that already
            // exists means re-resolving what is already in it — a set the fact
            // does not name, which is exactly what wide means.
            Fact::RecordingMerged { .. }
            | Fact::ReleaseGroupMerged { .. }
            | Fact::ArtistMerged { .. } => Blast::Wide,

            // **One entry.** `credits` is keyed by
            // `(subject, artist, role, source)`, so asserting a credit inserts
            // exactly what the fact names. That it decides `file_under` for
            // every track on a record (#84) is true of the *rows*, and rows
            // are rebuilt rather than patched.
            Fact::CreditAsserted { .. } | Fact::CreditRetracted { .. } => Blast::Narrow,

            // One group's title: `cat.groups.get_mut(..)`. The rows that show
            // it are rebuilt.
            Fact::ReleaseGroupRetitled { .. } => Blast::Narrow,

            // One edge, asserted or retracted. What climbing it does to a
            // `genre@1` heading (#79) is a question about a view, and a view
            // is rebuilt — **unless a closure table is stored**, at which
            // point these become wide and this arm has to say so. §5a: never
            // patch a derivation incrementally.
            Fact::HierarchyNarrowed { .. } | Fact::HierarchyNarrowRetracted { .. } => Blast::Narrow,

            // **Narrow since #193 normalised the row**, and the clearest case
            // of a tier following the projection rather than the fact.
            //
            // These were wide while a row stored `also_known_as` — a new alias
            // had to reach every row crediting the artist. A row now holds the
            // artist's **id** and the names are asked for when a filter needs
            // them, so an alias touches one artist record and no row at all.
            Fact::AliasAsserted { .. } | Fact::HeadingRetired { .. } => Blast::Narrow,

            // One edge in the artist graph, for the same reason as the
            // hierarchy above — and with the same caveat if it is ever given a
            // closure table.
            Fact::ArtistRelated { .. } => Blast::Narrow,

            // **Big, and not wide** — the distinction the tiers turn on.
            //
            // `HoldingsAsserted` carries every encoding a device holds, so
            // applying one is a lot of work: 62 of them account for 54 MB, 42%
            // of this library's log, because each is a full snapshot. But the
            // affected set is **exactly the keys of the map**, which the fact
            // hands over. It is N narrow updates arriving together, and a
            // projector needs no search to apply it.
            //
            // Wide means the affected set is *not derivable from the fact* —
            // `ArtistMerged` gives two ids and leaves "every row crediting the
            // loser" to be found. That is the thing that costs, and size is
            // not it.
            Fact::HoldingsAsserted { .. } => Blast::Narrow,
        }
    }
}

/// Fold one assignment, by the two tiers an assignment has.
///
/// The key is the claim's **identity** and it names who says so, so a second
/// asserter lands beside the first rather than on top of it and re-importing
/// one source is idempotent. The value is the claim's one **attribute**, and
/// only the same asserter can move it.
///
/// Clearing the retraction is the other half. Records arrive in stamp order,
/// so an assignment seen after one revives the pair — and a stale replica
/// replaying an older assignment never does.
pub fn compact(records: &BTreeSet<Record>) -> BTreeSet<Record> {
    let mut newest: BTreeMap<Id<Device>, Stamp> = BTreeMap::new();
    for Record { stamp, fact } in records {
        if let Fact::HoldingsAsserted { device, .. } = fact {
            let entry = newest.entry(*device).or_insert(*stamp);
            if *stamp > *entry {
                *entry = *stamp;
            }
        }
    }
    records
        .iter()
        .filter(|Record { stamp, fact }| match fact {
            Fact::HoldingsAsserted { device, .. } => newest.get(device) == Some(stamp),
            _ => true,
        })
        .cloned()
        .collect()
}

impl PartialOrd for Record {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// **By stamp, then by what it says** — #238.
///
/// The stamp leads, so iteration order is still chronological and total across devices,
/// and `flush`'s `stamp > flushed` watermark and `compact`'s newest-per-device both mean
/// what they meant.
///
/// **The fact is the tiebreak, and it is not decoration.** This compared stamps alone
/// while `Eq` compared both fields, which violates `Ord`'s documented requirement that
/// `cmp` answer `Equal` exactly when `==` does — and a `BTreeSet` believes `cmp`. So two
/// records with one stamp and different facts were *the same record* to the set holding
/// the whole log, and the second was **discarded in silence**:
///
/// ```text
/// engine stamp:   Stamp { wall: 1000, counter: 0, device: … }
/// importer stamp: Stamp { wall: 1000, counter: 0, device: … }
/// same stamp: true          records equal: false
/// two facts appended, 1 survived the union
/// ```
///
/// #218 made the two-appender case unreachable by taking an append lock, and this is
/// the other half: a stamp can still collide for reasons nothing can lock out — a clock
/// that went backwards past a counter reset, a `machine_id` restored onto two machines,
/// a replica synced from a device whose id was regenerated. In every one of those,
/// losing a fact without a word is the worst available outcome for a log whose whole
/// claim is that nothing is lost.
///
/// It also repairs `Changed::of`, which reads `records().difference(&before)`: a new
/// fact arriving on a taken stamp was *invisible* to the set difference, so the
/// projection would not have been told about it either.
impl Ord for Record {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.stamp
            .cmp(&other.stamp)
            .then_with(|| self.fact.cmp(&other.fact))
    }
}

#[cfg(test)]
mod reachability {
    /// **The rebuild tier is empty, and a test is what keeps it that way** —
    /// #192.
    ///
    /// A fact that quietly needs a full replay is a stutter nobody can predict.
    /// One that is *documented* to need it is a cost a person can be told
    /// about — so `Blast::Rebuild` may exist, and putting a fact in it is a
    /// decision somebody makes on purpose and has to change this test to make.
    #[test]
    fn no_fact_needs_a_rebuild() {
        let body = blast_body();
        assert!(
            !body.contains("Blast::Rebuild"),
            "a fact was classified as needing a full replay. That may be right, \
             but it is a cost the TUI pays on a keystroke, so say so here and in \
             access-layers.md rather than only in the match."
        );
    }

    /// **No wildcard arm, so the compiler's exhaustiveness check stays real.**
    ///
    /// `blast()` is enforced by being an exhaustive `match`: a new `Fact` does
    /// not compile until it has a tier. A single `_ => Blast::Narrow` would end
    /// that silently — and would classify every future fact as the *cheapest*
    /// tier, which is the one direction that leaves a projection wrong rather
    /// than slow.
    #[test]
    fn no_fact_is_classified_by_a_wildcard() {
        let body = blast_body();
        for line in body.lines().map(str::trim) {
            assert!(
                !line.starts_with("_ =>") && !line.starts_with("_ |"),
                "a wildcard arm would classify every future fact without anyone \
                 deciding: {line}"
            );
        }
    }

    /// **Every variant is named, so a grouping cannot swallow one.**
    ///
    /// The compiler proves the match is exhaustive; it does not prove that
    /// each variant was *considered*. Comparing the two lists is what says
    /// somebody looked at each one.
    #[test]
    fn every_fact_names_its_blast_radius() {
        let text = std::fs::read_to_string("src/facts/mod.rs").expect("log.rs");
        let start = text.find("pub enum Fact {").expect("the Fact enum");
        let body = &text[start..text[start..].find("\n}\n").unwrap() + start];
        let mut variants = Vec::new();
        for line in body.lines() {
            let Some(rest) = line.strip_prefix("    ") else {
                continue;
            };
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
            if name.is_empty() || !name.starts_with(char::is_uppercase) {
                continue;
            }
            if rest.starts_with(&format!("{name} {{")) || rest == format!("{name},") {
                variants.push(name);
            }
        }
        assert!(variants.len() > 40, "the enum was not parsed: {variants:?}");

        let blast = blast_body();
        let missing: Vec<_> = variants
            .iter()
            .filter(|v| {
                !blast.contains(&format!("Fact::{v} ")) && !blast.contains(&format!("Fact::{v}{{"))
            })
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "these facts are matched by a grouping rather than by name, so nobody \
             decided their blast radius: {missing:?}"
        );
    }

    /// **A colliding stamp keeps both facts rather than losing one** — #238.
    ///
    /// `Ord` compared stamps alone while `Eq` compared both fields, and a `BTreeSet`
    /// believes `cmp` — so two records with one stamp were the same record to the set
    /// holding the whole log, and the second went without a word.
    ///
    /// #218 locked out the way this happened in practice (two processes on one device
    /// id). A stamp can still collide for reasons nothing can lock out: a clock that
    /// went backwards past a counter reset, a `machine_id` restored onto two machines,
    /// a replica from a device whose id was regenerated.
    #[test]
    fn two_facts_on_one_stamp_both_survive_the_union() {
        let stamp = crate::model::clock::Stamp {
            wall: 1_000,
            counter: 0,
            device: Id::derived(b"one machine"),
        };
        let rated = Record {
            stamp,
            fact: Fact::RecordingRated {
                recording: Id::from_uuid(uuid::Uuid::from_u128(7)),
                stars: Some(5),
            },
        };
        let named = Record {
            stamp,
            fact: Fact::ArtistNamed {
                artist: Id::from_uuid(uuid::Uuid::from_u128(8)),
                name: "Aphex Twin".into(),
            },
        };
        assert_eq!(rated.stamp, named.stamp, "the collision is the premise");
        assert_ne!(rated, named, "and they are different records");

        let mut both: BTreeSet<Record> = BTreeSet::new();
        both.insert(rated.clone());
        both.insert(named.clone());
        assert_eq!(both.len(), 2, "one of two appended facts was discarded");
        assert!(both.contains(&rated) && both.contains(&named));

        // **The contract `Ord` documents**, which is what was actually broken: `cmp`
        // must answer `Equal` exactly when `==` does.
        assert_ne!(
            rated.cmp(&named),
            std::cmp::Ordering::Equal,
            "`cmp` calls them equal while `==` does not"
        );
        assert_eq!(rated.cmp(&rated.clone()), std::cmp::Ordering::Equal);
    }

    /// **The stamp still leads, so everything that reads order still reads it** — #238.
    ///
    /// `flush` takes records with `stamp > flushed` and `compact` keeps the newest
    /// holdings assertion per device; both would break if the fact sorted first. And
    /// iteration stays chronological, which is what makes a fold reproducible.
    #[test]
    fn ordering_is_chronological_first_and_the_fact_only_breaks_a_tie() {
        let device = Id::derived(b"one machine");
        let at = |wall: i64, counter: u32| crate::model::clock::Stamp {
            wall,
            counter,
            device,
        };
        // A fact that sorts *late* among facts, on an early stamp.
        let early = Record {
            stamp: at(1_000, 0),
            fact: Fact::RecordingRated {
                recording: Id::from_uuid(uuid::Uuid::from_u128(7)),
                stars: Some(5),
            },
        };
        // One that sorts *early* among facts, on a later stamp.
        let later = Record {
            stamp: at(2_000, 0),
            fact: Fact::ArtistNamed {
                artist: Id::from_uuid(uuid::Uuid::from_u128(8)),
                name: "Aphex Twin".into(),
            },
        };
        assert!(
            later.fact < early.fact,
            "the premise: these two facts sort the other way round"
        );
        assert!(early < later, "the stamp decides, not the fact");

        let ordered: Vec<i64> = BTreeSet::from([later, early])
            .into_iter()
            .map(|r| r.stamp.wall)
            .collect();
        assert_eq!(ordered, vec![1_000, 2_000], "iteration is chronological");
    }

    /// **Every fact in the live log survives the format the protocol parses into** —
    /// #240.
    ///
    /// The test that would have turned a panic into a failure. `serde_json::to_value`
    /// is what the wire's ingress builds before typing a line (I4), and `to_string` is
    /// what the log writes — and they are not the same: a `u128` writes exactly and
    /// cannot be a `Value`, so a fact carrying an audio id crossed the log fine and the
    /// wire not at all.
    ///
    /// Reading the *real* log rather than building 54 variants by hand, because that is
    /// the set that actually exists and the one whose shapes surprised us. Skipped when
    /// there is no log to read, which is every machine but this one — so it is a
    /// ratchet here and silent in CI, and `an_audio_id_survives_the_value_the_protocol_parses_into`
    /// in `gmuse-ids` is what holds the line everywhere.
    #[test]
    fn every_fact_in_the_log_survives_the_protocols_encoding() {
        let Some(dir) = std::env::var_os("HOME").map(|h| {
            std::path::PathBuf::from(h)
                .join("MusicLibrary")
                .join(".gmuse")
        }) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut checked = 0usize;
        let mut kinds: std::collections::BTreeSet<String> = Default::default();
        for path in entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                let Ok(record) = serde_json::from_str::<Record>(line) else {
                    continue;
                };
                // **The fact's own tag, taken from the fact** — not from the record's
                // first key, which is `stamp`. The first version of this did that and
                // so believed there was one kind of fact in the library: it checked a
                // single record and passed, which is the sixth time in one day.
                let written = serde_json::to_string(&record.fact).expect("the log wrote it");
                let name = written
                    .trim_start_matches('{')
                    .split('"')
                    .nth(1)
                    .unwrap_or("?")
                    .to_string();
                // One of each kind is enough, and the live log holds 280,000 records.
                if !kinds.insert(name.clone()) {
                    continue;
                }
                checked += 1;
                let value = serde_json::to_value(&record.fact).unwrap_or_else(|e| {
                    panic!("a {name} cannot be a `Value`, so it cannot cross the wire: {e}")
                });
                let back: Fact = serde_json::from_value(value)
                    .unwrap_or_else(|e| panic!("a {name} does not read back: {e}"));
                assert_eq!(
                    serde_json::to_string(&back).expect("encodes"),
                    written,
                    "a {name} changed going through a `Value`"
                );
            }
        }
        // **Says what it looked at.** A walk that found no log is the ordinary case off
        // this machine, and a silent pass over zero facts is the failure this project
        // has paid for five times.
        eprintln!("{checked} fact kind(s) round-tripped: {kinds:?}");
        // **Either it read a library or it read nothing.** Anything in between means
        // the parse is wrong, which is exactly how the first version of this passed
        // while looking at one record.
        assert!(
            checked == 0 || checked > 10,
            "only {checked} kind(s) came out of a real log: {kinds:?}"
        );
    }

    /// **Every variant says whether a view may propose it** — #236.
    ///
    /// The same ratchet as `every_fact_names_its_blast_radius` and for the same
    /// reason, with one extra stake: this list decides what a socket will accept
    /// into the log. A new variant swept up by a catch-all would be either
    /// silently writable by any client or silently unwritable by the UI, and
    /// neither failure announces itself.
    #[test]
    fn every_fact_says_whether_a_view_may_propose_it() {
        let variants = fact_variants();
        let body = body_of("    pub fn is_an_edit(&self) -> bool {");
        let missing: Vec<_> = variants
            .iter()
            .filter(|v| !body.contains(&format!("Fact::{v} ")))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "these facts are not named in `is_an_edit`, so nobody decided whether a \
             view may write them: {missing:?}"
        );
    }

    /// **The set is small, and a change to it should be noticed.**
    ///
    /// Not a restatement of the function: a count is what catches a `false` arm
    /// quietly becoming `true`, which is the direction that matters — it widens
    /// what a socket will take into the log.
    #[test]
    fn only_what_a_view_can_produce_is_an_edit() {
        let body = body_of("    pub fn is_an_edit(&self) -> bool {");
        // **The patterns of the arms that answer `true`.** Split on the arrow and
        // keep each left-hand side whose verdict is `true`: a chunk contains the
        // previous arm's verdict and this arm's patterns, which is exactly what is
        // wanted — a refused group's names sit in a chunk followed by `false`.
        let arms: Vec<&str> = body.split("=>").collect();
        let allowed: String = arms
            .windows(2)
            .filter(|w| w[1].trim_start().starts_with("true"))
            .map(|w| w[0])
            .collect();
        let named: Vec<String> = fact_variants()
            .into_iter()
            .filter(|v| allowed.contains(&format!("Fact::{v} ")))
            .collect();
        assert_eq!(
            named.len(),
            11,
            "the set a view may propose changed: {named:?}"
        );
    }

    /// The variant names of `Fact`, read from source.
    fn fact_variants() -> Vec<String> {
        let text = std::fs::read_to_string("src/facts/mod.rs").expect("facts/mod.rs");
        let start = text.find("pub enum Fact {").expect("the Fact enum");
        let body = &text[start..text[start..].find("\n}\n").unwrap() + start];
        let mut variants = Vec::new();
        for line in body.lines() {
            let Some(rest) = line.strip_prefix("    ") else {
                continue;
            };
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
            if name.is_empty() || !name.starts_with(char::is_uppercase) {
                continue;
            }
            if rest.starts_with(&format!("{name} {{")) || rest == format!("{name},") {
                variants.push(name);
            }
        }
        assert!(variants.len() > 50, "the enum was not parsed: {variants:?}");
        variants
    }

    /// The body of a function, read from source by its signature line.
    fn body_of(signature: &str) -> String {
        let text = std::fs::read_to_string("src/facts/mod.rs").expect("facts/mod.rs");
        let start = text
            .find(signature)
            .unwrap_or_else(|| panic!("{signature}"));
        let rest = &text[start..];
        let end = rest.find("\n    }\n").expect("the end of the function");
        rest[..end].to_string()
    }

    /// The body of `Fact::blast`, read from source.
    fn blast_body() -> String {
        let text = std::fs::read_to_string("src/facts/mod.rs").expect("log.rs");
        let start = text
            .find("    pub fn blast(&self) -> Blast {")
            .expect("blast()");
        let rest = &text[start..];
        let end = rest.find("\n    }\n").expect("the end of blast()");
        rest[..end].to_string()
    }

    use super::*;

    /// **A measurement written before the cliff was recorded still reads** —
    /// #184, and it is the reason the field is optional rather than a
    /// migration.
    ///
    /// 7,210 of these were in the live log when the field arrived. Refusing
    /// them would have made `durable.rs` report damage across the middle of a
    /// file, and defaulting them to a number would claim a measurement nobody
    /// took.
    #[test]
    fn a_measurement_without_a_cliff_reads_back_as_one_without_a_cliff() {
        let before = r#"{"EncodingMeasured":{"encoding":"00000000-0000-5000-8000-000000000001","cutoff_hz":15700,"analyser":1}}"#;
        let fact: Fact = serde_json::from_str(before).expect("a fact from before #184");
        let Fact::EncodingMeasured {
            cutoff_hz,
            cliff_db,
            ..
        } = fact
        else {
            panic!("wrong variant")
        };
        assert_eq!(cutoff_hz, 15_700, "the half that was recorded survives");
        assert_eq!(cliff_db, None, "and the half that was not says so");

        // Written today, both halves travel.
        let now = serde_json::to_string(&Fact::EncodingMeasured {
            encoding: Id::derived(b"e"),
            cutoff_hz: 15_700,
            cliff_db: Some(5_200),
            analyser: 1,
        })
        .expect("serialises");
        assert!(now.contains("\"cliff_db\":5200"), "{now}");
    }

    /// Which fact brings a kind into existence, if any.
    ///
    /// Hand-maintained on purpose: the point is not that it is derived, but
    /// that adding an entity without a fact to create it turns this red.
    ///
    /// **Reachable means every field of the row comes from a fact.** A
    /// `Holding` qualifies even though no fact is named for it: the fold reads
    /// `HoldingsAsserted { device, held }` and a row's encoding and locator are
    /// the map entry, its device is the fact's. Nothing is invented. A
    /// `Release` does not qualify — `from_catalogue` mints its id from a
    /// literal and copies its title off the group, so its *identity* is an
    /// invention that no device authored and no other device can agree with.
    const CREATED_BY: &[(&str, Option<&str>)] = &[
        ("artist", Some("ArtistNamed")),
        ("recording", Some("RecordingCatalogued")),
        ("encoding", Some("EncodingDescribed")),
        ("release-group", Some("ReleaseGroupCatalogued")),
        ("device", Some("HoldingsAsserted")),
        ("holding", Some("HoldingsAsserted")),
        // ── the gap. This list may only shrink. ────────────────────────────
        //
        // Genre, hierarchy and genre-assignment left it when the vocabulary
        // facts landed; the spine is what remains.
        //
        // Every kind below is built by `Librarian::from_catalogue` out of thin
        // air: with no stamp, no basis, no author, no way to correct it and no
        // way to sync it. The catalogue is supposed to be a fold of the log,
        // and today it is a fold *plus* a projection that invents structure.
        //
        // Each of these needs a fact before anything else names it, or there
        // will be two producers for one row.
        ("work", Some("WorkCatalogued")),
        ("release", Some("ReleaseCatalogued")),
        ("medium", Some("MediumCatalogued")),
        ("track", Some("TrackPlaced")),
        ("label", Some("LabelNamed")),
        ("facet", Some("FacetDefined")),
        ("concept", Some("ConceptCatalogued")),
        ("hierarchy", Some("HierarchyDefined")),
        ("role", Some("RoleDefined")),
        ("collection", Some("CollectionNamed")),
        ("view", Some("ViewSaved")),
        ("credit", Some("CreditAsserted")),
        ("mix-entry", None),
        // A row rather than an entity: the fold keys an entry by
        // `(collection, position, recording)`, which is what lets one
        // recording appear in one playlist twice. Listed here because the
        // fact is what brings it into being, and nothing else does (#92).
        ("collection-entry", Some("CollectionEntryAdded")),
        ("assertion", Some("ConceptAsserted")),
    ];

    /// **The allowlist only shrinks.**
    ///
    /// A kind with no fact cannot be created by folding the log, so whatever
    /// produces it is producing something the library cannot record, correct,
    /// attribute or sync. Filling one in is progress; adding one is a
    /// regression, and this is where it is caught.
    #[test]
    fn every_entity_is_reachable_from_a_fact_or_named_as_a_gap() {
        // **One**, and it is `mix-entry`. `collection` and `collection-entry`
        // closed with #92: a playlist is a sequence of facts now rather than a
        // file on disk, so the log can record, correct, attribute and sync one.
        const UNREACHABLE: usize = 1;

        let kinds = crate::model::entity::ALL_KINDS;

        let listed: BTreeSet<&str> = CREATED_BY.iter().map(|(k, _)| *k).collect();
        let all: BTreeSet<&str> = kinds.iter().copied().collect();
        assert_eq!(
            all.difference(&listed).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "a kind exists that this table does not mention — add it, with or without a fact"
        );
        assert_eq!(
            listed.difference(&all).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "the table mentions a kind that no longer exists"
        );

        let gaps: Vec<&str> = CREATED_BY
            .iter()
            .filter(|(_, fact)| fact.is_none())
            .map(|(k, _)| *k)
            .collect();
        assert!(
            gaps.len() <= UNREACHABLE,
            "the gap widened: {} kinds have no fact (was {UNREACHABLE}) — {gaps:?}",
            gaps.len()
        );
        if gaps.len() < UNREACHABLE {
            eprintln!("gap narrowed to {} — lower UNREACHABLE", gaps.len());
        }
    }

    /// The named facts are real. Without this the table could drift into
    /// naming variants that no longer exist and still pass.
    #[test]
    fn the_facts_the_table_names_all_exist() {
        let source = include_str!("mod.rs");
        let start = source.find("pub enum Fact {").expect("the Fact enum");
        let body = &source[start..start + source[start..].find("\n}\n").expect("its close")];
        for (kind, fact) in CREATED_BY {
            let Some(name) = fact else { continue };
            assert!(
                body.contains(&format!("\n    {name} {{")),
                "{kind} claims to be created by {name}, which is not a Fact variant"
            );
        }
    }
}
