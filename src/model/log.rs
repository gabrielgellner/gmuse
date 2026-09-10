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
use crate::model::entity::{
    Artist, Collection, Concept, Device, Encoding, Facet, Hierarchy, Label, Medium, Recording,
    Release, ReleaseGroup, Role, Track, View, Work,
};
use crate::model::facet::{ConceptRecord, Described, FacetRecord};
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        /// It stays until #78 dissolves `Track`, because the tree projection
        /// and `Row` still read it. When they ask for a credit instead, this
        /// goes, and until then **the credit is authoritative**: anything that
        /// needs to be right reads `credits`, and this is the fast path for
        /// the two callers that predate it.
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
fn assign<S, C>(
    live: &mut BTreeMap<(S, C, Source), Confidence>,
    retracted: &mut BTreeMap<(S, C), Source>,
    subject: S,
    concept: C,
    basis: Source,
    confidence: Confidence,
) where
    S: Ord + Copy,
    C: Ord + Copy,
{
    // **Recorded whether or not it is shadowed.** Dropping it would destroy
    // the provenance saying where it came from, which is the thing that makes
    // an override inspectable rather than magic (#96).
    live.insert((subject, concept, basis), confidence);
    // **Precedence by basis** (#96). An import does not undo a person's
    // correction: without this, retracting a wrong Discogs genre lasts exactly
    // until the next Discogs pass, and an edit that does not survive a resync
    // is not an edit.
    if retracted
        .get(&(subject, concept))
        .is_none_or(|by| basis >= *by)
    {
        retracted.remove(&(subject, concept));
    }
}

/// Fold one retraction, **keyed one field shorter than what it shadows**.
///
/// A retraction answers the question rather than arguing with one asserter, so
/// it silences every basis at once. Keyed by basis instead, a person would have
/// to retract once per source that ever said it and the next import would put
/// it back.
///
/// The row is kept rather than dropped, because *decided against* and *nobody
/// has said* are different answers and only a row can tell them apart.
fn retract<S, C>(
    live: &mut BTreeMap<(S, C, Source), Confidence>,
    retracted: &mut BTreeMap<(S, C), Source>,
    subject: S,
    concept: C,
    basis: Source,
) where
    S: Ord + Copy,
    C: Ord + Copy,
{
    live.retain(|(s, c, _), _| (*s, *c) != (subject, concept));
    // The strongest voice that ruled it out, so a weaker retraction arriving
    // later cannot weaken a person's.
    let at = retracted.entry((subject, concept)).or_insert(basis);
    *at = (*at).max(basis);
}

/// Fold one claim that has no attribute to carry.
///
/// The row-existence form of [`assign`]: where an assignment splits into an
/// identity and an attribute, an edge is identity all the way down — there is
/// nothing to say about `Techno narrows Electronic` beyond that somebody said
/// it. So the key is the whole claim, and the fold is a set union.
fn assert_edge<K: Ord + Clone>(live: &mut BTreeSet<K>, retracted: &mut BTreeSet<K>, edge: K) {
    retracted.remove(&edge);
    live.insert(edge);
}

/// Fold one retraction of such a claim — the same rule as [`retract`].
///
/// The live row goes, a row saying it was **decided against** stays, and a
/// later assertion revives. Keeping the row is what separates "somebody ruled
/// this out" from "nobody has said", which an absence cannot express and a
/// review queue needs.
///
/// It differs from [`retract`] in exactly one place, and only because the
/// claims differ: an assignment's live key carries the basis that made it, so
/// retracting one has to sweep every asserter. An edge has no asserter in its
/// key, so there is one row to remove.
fn retract_edge<K: Ord + Clone>(live: &mut BTreeSet<K>, retracted: &mut BTreeSet<K>, edge: K) {
    live.remove(&edge);
    retracted.insert(edge);
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
    pub encodings: BTreeMap<Id<Encoding>, Encoded>,
    /// Which recording each encoding holds, **independent of whether anything
    /// has described it**.
    ///
    /// `Encoded::recording` carries the same answer for encodings a scan has
    /// seen, and is what the projection reads. This is the one that survives a
    /// log with no scan beside it — the case a relink lives in.
    pub bound: BTreeMap<Id<Encoding>, Id<Recording>>,
    /// Every assertion, on every axis, **one row per asserter**.
    ///
    /// The key is the claim's identity and it includes *who says so*, because
    /// Discogs saying Techno and a person saying Techno are two independent
    /// pieces of evidence rather than one claim written twice. Folding them to
    /// one row would make the second silently overwrite the first and lose the
    /// agreement, which is the most useful thing the pair says.
    ///
    /// The value is the claim's one **attribute**: last writer wins per
    /// identity, so re-scoring an import (Discogs moving provisional to matched)
    /// updates in place, while a human's disagreement lands beside it.
    ///
    /// The facet is not in the key because it is already in the concept's id
    /// (#117) — `ambient` on two axes is two concepts, so one map holds both
    /// without a third field to keep in step.
    pub assertions: BTreeMap<(Described, Id<Concept>, Source), Confidence>,
    /// Pairs somebody has ruled out.
    ///
    /// **Keyed without a basis, unlike the assertions it shadows.** A person
    /// saying "this is not live" is answering the question, not arguing with
    /// one asserter, so a retraction silences every basis at once — otherwise
    /// retracting would mean retracting once per source that ever said it, and
    /// the next import would put it back.
    ///
    /// Kept rather than dropped, because *decided against* and *nobody has
    /// said* are different answers and only a row can tell them apart. A later
    /// assertion clears the entry, or a retraction would be a delete wearing
    /// another name.
    pub assertions_retracted: BTreeMap<(Described, Id<Concept>), Source>,
    /// Every saved view, by the id its name mints.
    pub views: BTreeMap<Id<View>, SavedView>,
    /// Views somebody has dropped. Kept, because *deleted* and *never existed*
    /// are different answers and only a row can tell them apart.
    pub views_dropped: BTreeSet<Id<View>>,
    /// What each collection is called, and whether its order means anything.
    pub collections: BTreeMap<Id<Collection>, Collected>,
    /// Every collection entry, **keyed by position before recording**.
    ///
    /// So that walking the set gives a collection's entries in list order —
    /// a `BTreeSet` over `(collection, ord, recording)` is already sorted the
    /// way the playlist reads, and the fold needs no comparator.
    ///
    /// Two devices inserting *different* recordings at the same point produce
    /// equal `ord`s, which is fine: the recording breaks the tie, and it
    /// breaks it the same way on both machines.
    pub entries: BTreeSet<(Id<Collection>, OrdKey, Id<Recording>)>,
    /// Entries somebody has taken out. Kept rather than deleted, for the same
    /// reason every other retraction is: an absence cannot tell "removed" from
    /// "never added", and a merge has to be able to.
    pub entries_retracted: BTreeSet<(Id<Collection>, OrdKey, Id<Recording>)>,
    /// Every other name anything is known by. Row existence, so curation
    /// unions across devices the way `narrows` does.
    pub aliases: BTreeSet<(Cataloguable, String, AliasKind)>,
    /// Every name anything was authoritatively called and is not called now
    /// — `Fact::HeadingRetired`. A subset of `aliases`, held apart rather than
    /// flagged inside it, because MADS/RDF's `DeprecatedAuthority` is a
    /// disjoint class from `Variant` and the two axes multiply.
    #[serde(default)]
    pub retired_headings: BTreeSet<(Cataloguable, String)>,
    /// One artist to another — `Fact::ArtistRelated`. Row existence, so two
    /// devices stating one relation union.
    ///
    /// **A graph, not a redirect.** `Merges` resolves a chain and the far end
    /// replaces the near one; here both ends stay and only a *view* collapses
    /// them, so the edges are kept as edges.
    #[serde(default)]
    pub artist_relations: BTreeSet<ArtistEdge>,
    /// When a relation held, where anybody said — keyed by the edge.
    ///
    /// Apart from the edge itself because it is the one part that can be
    /// restated: a membership whose end date arrives later is the same edge
    /// with more known about it, and folding it into the key would make it a
    /// second edge instead.
    #[serde(default)]
    pub relation_periods: BTreeMap<ArtistEdge, Period>,
    /// Every axis of description there is.
    pub facets: BTreeMap<Id<Facet>, FacetRecord>,
    /// Every term, on whichever axis it sits.
    ///
    /// **One list for every facet** (#117), where genre and version kept two.
    /// Nothing can collide: a concept's id seeds from its facet's key, so
    /// `ambient` the style and `ambient` the function are different rows.
    pub concepts: BTreeMap<Id<Concept>, ConceptRecord>,
    /// What each imprint is called.
    pub label_names: BTreeMap<Id<Label>, String>,
    /// ReplayGain per encoding, as the tags claim it.
    pub levels: BTreeMap<Id<Encoding>, crate::model::encoding::Levels>,
    /// What each hierarchy is called, and which axis it groups.
    pub hierarchies: BTreeMap<Id<Hierarchy>, (Id<Facet>, String)>,
    /// Every narrower-of edge. Row existence, so curation unions.
    pub narrows: BTreeSet<(Id<Hierarchy>, Id<Concept>, Id<Concept>)>,
    /// Edges somebody has ruled out. Kept for the reason
    /// [`Catalogue::assertions_retracted`] is: a decision and a silence are
    /// different answers, and only a row can tell them apart.
    pub narrows_retracted: BTreeSet<(Id<Hierarchy>, Id<Concept>, Id<Concept>)>,
    /// Every bytes-came-from-bytes edge — #132. Row existence, like the rest.
    pub encoding_derived: BTreeSet<(Id<Encoding>, Id<Encoding>)>,
    pub encoding_derived_retracted: BTreeSet<(Id<Encoding>, Id<Encoding>)>,
    /// Every recording-derived-from edge. Row existence, so curation unions
    /// exactly as `narrows` does (#79).
    pub derived: BTreeSet<(
        Id<Recording>,
        Id<Recording>,
        crate::model::spine::RecordingDerivationKind,
    )>,
    pub derived_retracted: BTreeSet<(
        Id<Recording>,
        Id<Recording>,
        crate::model::spine::RecordingDerivationKind,
    )>,
    /// The other traversal, one level up: work-derived-from-work.
    pub work_derived: BTreeSet<(
        Id<crate::model::entity::Work>,
        Id<crate::model::entity::Work>,
        crate::model::spine::WorkDerivationKind,
    )>,
    pub work_derived_retracted: BTreeSet<(
        Id<crate::model::entity::Work>,
        Id<crate::model::entity::Work>,
        crate::model::spine::WorkDerivationKind,
    )>,
    pub plays: BTreeMap<Id<Recording>, u32>,
    /// What each recording's identity was derived from.
    pub identified: BTreeMap<Id<Recording>, crate::model::basis::Basis>,
    /// When each recording entered the library, as the earliest anyone says.
    pub acquired: BTreeMap<Id<Recording>, i64>,
    /// The most recent play, in **seconds** since the epoch — the same unit
    /// as `acquired` and as the filter language's clock, not the stamp's.
    pub last_played: BTreeMap<Id<Recording>, i64>,
    /// The latest rating per recording. A fold, never a stored number.
    pub ratings: BTreeMap<Id<Recording>, u8>,
    /// Cutoff and the analyser that found it, per encoding.
    pub measurements: BTreeMap<Id<Encoding>, Spectrum>,
    pub groups: BTreeMap<Id<ReleaseGroup>, Grouped>,
    /// Which recordings have been folded into which.
    ///
    /// Applied to **history** — ratings, plays, acquisition — and to anything
    /// else keyed by a recording, so a merge moves what was said about the
    /// loser rather than stranding it.
    pub merges: crate::model::merge::Merges<Recording>,
    /// The same, for albums — #156. Kept apart because they answer different
    /// questions and a merge at one level implies nothing at the other.
    pub group_merges: crate::model::merge::Merges<ReleaseGroup>,
    /// The redirects an artist merge left — `Fact::ArtistMerged`.
    pub artist_merges: crate::model::merge::Merges<Artist>,
    /// **Bytes the library holds and has not named** — #160.
    ///
    /// What the tags claimed, for an encoding bound to no recording. Empty for
    /// every file an authority names, because those are catalogued outright.
    pub claims: BTreeMap<Id<Encoding>, (String, String)>,
    /// Every composition anyone has named.
    pub works: BTreeMap<Id<Work>, String>,
    /// Which recordings perform which works. Row existence: a recording can
    /// perform two, which is what a medley is.
    pub performances: BTreeSet<(Id<Recording>, Id<Work>)>,
    /// The role codelist.
    pub roles: BTreeMap<Id<Role>, (String, RoleClass)>,
    /// Every credit. Keyed by the claim, valued by how it was printed.
    pub credits: BTreeMap<(CreditOn, Id<Artist>, Id<Role>, Source), Credited>,
    /// Credits somebody has ruled out, and who ruled them out — the shadow
    /// [`Fact::CreditRetracted`] casts. Keyed without the `Source`, because a
    /// retraction answers the question rather than one asserter.
    pub credits_retracted: BTreeMap<(CreditOn, Id<Artist>, Id<Role>), Source>,
    /// Every pressing, by id.
    pub releases: BTreeMap<Id<Release>, Released>,
    /// Every disc, by id.
    pub media: BTreeMap<Id<Medium>, Pressed>,
    /// Every slot: which medium, which recording, which number.
    pub tracks: BTreeMap<Id<Track>, Placed>,
    /// Which slot each copy sits in. Row existence — a file is in one place,
    /// and two devices holding the same file agree about where.
    pub placed: BTreeMap<Id<Encoding>, Id<Track>>,
    /// Which copies a mastering tool re-encoded, and who says so — #143.
    ///
    /// A set of bases rather than one, because claims accumulate: the path
    /// saying it and a tagger saying it are two pieces of evidence, and their
    /// agreeing is the most useful thing the pair says.
    pub processed: BTreeMap<Id<Encoding>, BTreeSet<crate::model::reference::Source>>,
    /// Copies the library has retired — #159. Their rows are all still here,
    /// which is what makes the retirement reversible; what changes is that
    /// nothing counts them as a copy the library holds.
    pub retired: BTreeSet<Id<Encoding>>,
    /// What each device last said it holds, and when it said so.
    ///
    /// The stamp is not decoration. You cannot stat another machine's disk, so
    /// a remote holding is the claim "device X had this, there, as of X's last
    /// sync" — and a row that reads "on asnas, as of Tuesday" is honest where
    /// one that reads "on asnas" is not.
    pub holdings: BTreeMap<Id<Device>, Asserted>,
    /// Structural disagreements. Never resolved silently — §4.
    pub conflicts: Vec<Conflict>,
}

/// One device's holdings, as of one moment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Asserted {
    pub at: Stamp,
    pub held: BTreeMap<Id<Encoding>, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Credited {
    pub position: u8,
    pub as_credited: Option<String>,
    pub join_phrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Released {
    pub group: Id<ReleaseGroup>,
    pub title: String,
    pub released: Option<PartialDate>,
    pub label: Option<Id<Label>>,
    #[serde(default)]
    pub media: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pressed {
    pub release: Id<Release>,
    pub position: u16,
    pub format: Option<String>,
    #[serde(default)]
    pub tracks: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placed {
    pub medium: Id<Medium>,
    pub recording: Id<Recording>,
    pub position: u16,
    pub title_on_release: Option<String>,
}

/// A saved view, as the fold has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedView {
    pub name: String,
    /// The stack as somebody typed it — `in="Jazz" &year>=2015`.
    pub source: String,
}

/// A collection, as the fold has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Collected {
    pub name: String,
    /// Whether the order means anything. A playlist is an ordered collection;
    /// an unordered one is a set somebody curated by hand.
    pub ordered: bool,
    /// The query it was frozen from, if it was frozen rather than hand-built.
    ///
    /// `None` is "somebody made this by hand", which is a different answer
    /// from "frozen from a query that happens to match nothing" — and only a
    /// distinct value can tell them apart.
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Grouped {
    pub title: String,
    pub artist: Id<Artist>,
    pub first_released: Option<PartialDate>,
    #[serde(default)]
    pub kind: crate::model::spine::ReleaseType,
    #[serde(default)]
    pub secondary: std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recorded {
    pub title: String,
    pub artist: Id<Artist>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Encoded {
    pub audio: AudioId,
    pub recording: Option<Id<Recording>>,
    pub format: Option<crate::model::encoding::Format>,
    pub bitrate: u32,
    /// Playing time in milliseconds. Zero means nobody has said.
    pub length_ms: u32,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Two devices bound one encoding to different recordings, from the same base.
///
/// The winner is shown, because a library that refuses to answer is worse than
/// one that answers and says so. The losers are kept, because "stop and show
/// me" needs something to show.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub encoding: Id<Encoding>,
    pub base: Option<Id<Recording>>,
    /// Every value asserted, ordered by stamp. The last one is what shows.
    pub asserted: Vec<(Stamp, Id<Recording>)>,
}

impl Conflict {
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — a conflict's winner and losers, which no command surfaces"
    )]
    pub fn winner(&self) -> Id<Recording> {
        self.asserted.last().expect("a conflict has candidates").1
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — conflicts, the genre backlog and compaction, none of which a command surfaces"
    )]
    /// The recordings this collision left behind.
    ///
    /// One encoding cannot be two performances, so every loser here is the
    /// same song as the winner under a different id — which is a **merge**
    /// waiting to be proposed, not rubbish waiting to be collected.
    pub fn losers(&self) -> impl Iterator<Item = Id<Recording>> + '_ {
        let winner = self.winner();
        self.asserted
            .iter()
            .map(|(_, r)| *r)
            .filter(move |r| *r != winner)
    }
}

/// Every recording asserted for one `(encoding, base)` pair, in stamp order.
/// Grouping by the **base** is what distinguishes a disagreement from a
/// correction: two writes from the same base disagree, where a write from a
/// base someone else established is a correction of it.
type Bindings = BTreeMap<(Id<Encoding>, Option<Id<Recording>>), Vec<(Stamp, Id<Recording>)>>;

/// Fold a set of records into the catalogue they describe.
///
/// Sorting first is what makes the result independent of the order records
/// arrived in, which is the property the whole no-coordinator design rests on:
/// merge A into B or B into A and the answer is the same.
pub fn fold(records: &BTreeSet<Record>) -> Catalogue {
    let mut cat = Catalogue::default();
    let mut bindings: Bindings = BTreeMap::new();
    // Every placement each copy has ever been given, in stamp order, so the
    // pass below can prefer the best-evidenced rather than the last.
    let mut claimed: BTreeMap<Id<Encoding>, Vec<(Id<Track>, Source)>> = BTreeMap::new();

    // **Merges first, in their own pass.** A merge recorded on Tuesday has to
    // move a rating given on Monday, so it cannot be applied as it is reached
    // — every recording named below is resolved through the finished set.
    for Record { fact, .. } in records {
        match fact {
            Fact::RecordingMerged { loser, winner } => {
                let _ = cat.merges.merge(*loser, *winner);
            }
            Fact::ReleaseGroupMerged { loser, winner } => {
                let _ = cat.group_merges.merge(*loser, *winner);
            }
            Fact::ArtistMerged { loser, winner } => {
                let _ = cat.artist_merges.merge(*loser, *winner);
            }
            _ => {}
        }
    }
    let of = |r: &Id<Recording>| cat.merges.resolve(*r);
    let group_of = |g: &Id<ReleaseGroup>| cat.group_merges.resolve(*g);
    // **Every artist named below**, because a merge has to move a credit given
    // before it just as it moves a rating. The name goes through too: the
    // loser's label lands on the winner and the last writer wins, which is the
    // same rule `RecordingCatalogued`'s title already follows through `of`.
    let who = |a: &Id<Artist>| cat.artist_merges.resolve(*a);
    let described = |s: &Described| match s {
        Described::Recording(r) => Described::Recording(cat.merges.resolve(*r)),
        // A genre asserted on the loser is a genre on the album — #156.
        Described::ReleaseGroup(g) => Described::ReleaseGroup(cat.group_merges.resolve(*g)),
    };
    let credit_on = |s: &CreditOn| match s {
        CreditOn::Recording(r) => CreditOn::Recording(cat.merges.resolve(*r)),
        // And so is the album credit, which is what `file_under` asks for
        // first: a merge that left it behind would file the two halves of one
        // record under different artists.
        CreditOn::ReleaseGroup(g) => CreditOn::ReleaseGroup(cat.group_merges.resolve(*g)),
        other => *other,
    };

    for Record { stamp, fact } in records {
        match fact {
            Fact::ArtistNamed { artist, name } => {
                // Last writer wins, and the set is already in stamp order.
                cat.artist_names.insert(who(artist), name.clone());
            }
            Fact::RecordingCatalogued {
                recording,
                title,
                artist,
            } => {
                cat.recordings.insert(
                    of(recording),
                    Recorded {
                        title: title.clone(),
                        artist: who(artist),
                    },
                );
            }
            Fact::EncodingDescribed {
                encoding,
                audio,
                format,
                bitrate,
                length_ms,
                sample_rate,
                channels,
            } => {
                cat.encodings.entry(*encoding).or_insert(Encoded {
                    audio: *audio,
                    recording: None,
                    format: *format,
                    bitrate: *bitrate,
                    length_ms: *length_ms,
                    sample_rate: *sample_rate,
                    channels: *channels,
                });
            }
            Fact::EncodingBound { encoding, from, to } => {
                bindings
                    .entry((*encoding, from.map(|f| of(&f))))
                    .or_default()
                    .push((*stamp, of(to)));
            }
            Fact::ViewSaved { view, name, source } => {
                cat.views.insert(
                    *view,
                    SavedView {
                        name: name.clone(),
                        source: source.clone(),
                    },
                );
                cat.views_dropped.remove(view);
            }
            Fact::ViewDropped { view } => {
                cat.views.remove(view);
                cat.views_dropped.insert(*view);
            }
            Fact::CollectionNamed {
                collection,
                name,
                ordered,
            } => {
                // The source survives a rename: it is a claim about where the
                // membership came from, not about what the list is called.
                let from = cat.collections.get(collection).and_then(|c| c.from.clone());
                cat.collections.insert(
                    *collection,
                    Collected {
                        name: name.clone(),
                        ordered: *ordered,
                        from,
                    },
                );
            }
            Fact::CollectionFrozen { collection, source } => {
                if let Some(c) = cat.collections.get_mut(collection) {
                    c.from = Some(source.clone());
                }
            }
            Fact::CollectionEntryAdded {
                collection,
                recording,
                ord,
            } => assert_edge(
                &mut cat.entries,
                &mut cat.entries_retracted,
                (*collection, ord.clone(), of(recording)),
            ),
            Fact::CollectionEntryRemoved {
                collection,
                recording,
                ord,
            } => retract_edge(
                &mut cat.entries,
                &mut cat.entries_retracted,
                (*collection, ord.clone(), of(recording)),
            ),
            Fact::FacetDefined { facet, key, label } => {
                cat.facets.insert(
                    *facet,
                    FacetRecord {
                        key: key.clone(),
                        label: label.clone(),
                    },
                );
            }
            Fact::ConceptCatalogued {
                facet,
                concept,
                label,
                node_label,
            } => {
                cat.concepts.insert(
                    *concept,
                    ConceptRecord {
                        facet: *facet,
                        label: label.clone(),
                        node_label: *node_label,
                    },
                );
            }
            Fact::HierarchyDefined {
                hierarchy,
                facet,
                name,
            } => {
                cat.hierarchies.insert(*hierarchy, (*facet, name.clone()));
            }
            Fact::EncodingDerived { from, to } => assert_edge(
                &mut cat.encoding_derived,
                &mut cat.encoding_derived_retracted,
                (*from, *to),
            ),
            Fact::EncodingDerivationRetracted { from, to } => retract_edge(
                &mut cat.encoding_derived,
                &mut cat.encoding_derived_retracted,
                (*from, *to),
            ),
            Fact::RecordingDerived { from, to, kind } => assert_edge(
                &mut cat.derived,
                &mut cat.derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::RecordingDerivationRetracted { from, to, kind } => retract_edge(
                &mut cat.derived,
                &mut cat.derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::WorkDerived { from, to, kind } => assert_edge(
                &mut cat.work_derived,
                &mut cat.work_derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::WorkDerivationRetracted { from, to, kind } => retract_edge(
                &mut cat.work_derived,
                &mut cat.work_derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::HierarchyNarrowed {
                hierarchy,
                parent,
                child,
            } => assert_edge(
                &mut cat.narrows,
                &mut cat.narrows_retracted,
                (*hierarchy, *parent, *child),
            ),
            Fact::HierarchyNarrowRetracted {
                hierarchy,
                parent,
                child,
            } => retract_edge(
                &mut cat.narrows,
                &mut cat.narrows_retracted,
                (*hierarchy, *parent, *child),
            ),
            Fact::ConceptAsserted {
                subject,
                concept,
                basis,
                confidence,
            } => assign(
                &mut cat.assertions,
                &mut cat.assertions_retracted,
                described(subject),
                *concept,
                *basis,
                *confidence,
            ),
            Fact::AliasAsserted {
                subject,
                value,
                kind,
            } => {
                cat.aliases.insert((*subject, value.clone(), *kind));
            }
            // Both, and deliberately: a heading the library retired is still a
            // name it answers to, so it lands in `aliases` too — as an
            // `ArtistName`, because it was the authorised one and a printed
            // name is what it stays.
            Fact::HeadingRetired { subject, value } => {
                cat.aliases
                    .insert((*subject, value.clone(), AliasKind::ArtistName));
                cat.retired_headings.insert((*subject, value.clone()));
            }
            Fact::ArtistRelated {
                from,
                to,
                kind,
                period,
            } => {
                // **Both ends resolved through the merges**, because a merge
                // that ran after the relation was stated would otherwise
                // leave an edge pointing at a row nothing else uses.
                let edge = (
                    cat.artist_merges.resolve(*from),
                    cat.artist_merges.resolve(*to),
                    *kind,
                );
                cat.artist_relations.insert(edge);
                if let Some(span) = period {
                    cat.relation_periods.insert(edge, *span);
                }
            }
            Fact::ConceptRetracted {
                subject,
                concept,
                basis,
            } => retract(
                &mut cat.assertions,
                &mut cat.assertions_retracted,
                described(subject),
                *concept,
                *basis,
            ),
            Fact::HoldingsAsserted { device, held } => {
                // Records arrive in stamp order, so the last one simply wins.
                cat.holdings.insert(
                    *device,
                    Asserted {
                        at: *stamp,
                        held: held.clone(),
                    },
                );
            }
            Fact::ReleaseGroupCatalogued {
                group,
                title,
                artist,
                first_released,
                kind,
                secondary,
            } => {
                cat.groups.insert(
                    // **Through the merge**, so the loser keeps no album row
                    // of its own — the same shape `RecordingCatalogued` takes,
                    // and what stops a merged album still drawing a node.
                    group_of(group),
                    Grouped {
                        title: title.clone(),
                        // **Through the merge too**, and this is the one that
                        // reaches a folder: `file_under` asks the group for its
                        // artist first, so a split left here files the two
                        // halves of one artist's record apart.
                        artist: who(artist),
                        first_released: *first_released,
                        kind: *kind,
                        secondary: secondary.clone(),
                    },
                );
            }
            Fact::WorkCatalogued { work, title } => {
                cat.works.insert(*work, title.clone());
            }
            Fact::WorkPerformed { recording, work } => {
                cat.performances.insert((of(recording), *work));
            }
            Fact::RoleDefined { role, name, class } => {
                cat.roles.insert(*role, (name.clone(), *class));
            }
            Fact::CreditAsserted {
                subject,
                artist,
                role,
                basis,
                position,
                as_credited,
                join_phrase,
            } => {
                // **A later assertion lifts the shadow**, because the two
                // are one question answered twice and the later answer stands.
                cat.credits_retracted
                    .remove(&(credit_on(subject), who(artist), *role));
                cat.credits.insert(
                    // **The artist through the merge**, which is what collapses
                    // the two halves of a split credit into one row rather than
                    // leaving `line_of` to render the same person twice.
                    (credit_on(subject), who(artist), *role, *basis),
                    Credited {
                        position: *position,
                        as_credited: as_credited.clone(),
                        join_phrase: join_phrase.clone(),
                    },
                );
            }
            // Applied in the pass below, because a retitle has to outlast a
            // scan rather than merely follow it.
            Fact::RecordingRetitled { .. } | Fact::ReleaseGroupRetitled { .. } => {}
            Fact::EncodingClaimed {
                encoding,
                artist,
                title,
            } => {
                // Last writer wins, as every descriptive fact does: a retag
                // that changes what the file says changes what it claims.
                cat.claims
                    .insert(*encoding, (artist.clone(), title.clone()));
            }
            Fact::CreditRetracted {
                subject,
                artist,
                role,
                basis,
            } => {
                let key = (credit_on(subject), who(artist), *role);
                // Every basis at once — see the fact's own doc for why it is
                // keyed one field shorter than the assertion it shadows.
                cat.credits.retain(|(s, a, r, _), _| (*s, *a, *r) != key);
                // The strongest voice that ruled it out, so a weaker
                // retraction arriving later cannot weaken a person's.
                let at = cat.credits_retracted.entry(key).or_insert(*basis);
                *at = (*at).max(*basis);
            }
            Fact::EncodingPlaced {
                encoding,
                track,
                basis,
            } => {
                // Kept in full and resolved below, because the last word is
                // not always the best one — see the pass after this loop.
                cat.placed.insert(*encoding, *track);
                claimed.entry(*encoding).or_default().push((*track, *basis));
            }
            // **Last word wins**, because these come in a pair: a copy retired
            // and brought back and retired again is retired, and the set is
            // already in stamp order.
            Fact::EncodingProcessed { encoding, basis } => {
                cat.processed.entry(*encoding).or_default().insert(*basis);
            }
            Fact::EncodingRetired { encoding } => {
                cat.retired.insert(*encoding);
            }
            Fact::EncodingRestored { encoding } => {
                cat.retired.remove(encoding);
            }
            Fact::EncodingLevelled { encoding, levels } => {
                // Last writer wins: re-tagging a file is a correction, and
                // there is nothing to reconcile between two claims about one
                // encoding's loudness.
                cat.levels.insert(*encoding, *levels);
            }
            Fact::LabelNamed { label, name } => {
                cat.label_names.insert(*label, name.clone());
            }
            Fact::ReleaseCatalogued {
                release,
                group,
                title,
                released,
                label,
                media,
            } => {
                cat.releases.insert(
                    *release,
                    Released {
                        // **The pressing stays; only which album it is on
                        // moves.** That is the whole point of merging at this
                        // level: *Barton Hollow* is one album holding a
                        // standard edition and a deluxe, and folding the
                        // releases together would lose the difference.
                        group: group_of(group),
                        title: title.clone(),
                        released: *released,
                        label: *label,
                        media: *media,
                    },
                );
            }
            Fact::MediumCatalogued {
                medium,
                release,
                position,
                format,
                tracks,
            } => {
                cat.media.insert(
                    *medium,
                    Pressed {
                        release: *release,
                        position: *position,
                        format: format.clone(),
                        tracks: *tracks,
                    },
                );
            }
            Fact::TrackPlaced {
                track,
                medium,
                recording,
                position,
                title_on_release,
            } => {
                cat.tracks.insert(
                    *track,
                    Placed {
                        medium: *medium,
                        recording: of(recording),
                        position: *position,
                        title_on_release: title_on_release.clone(),
                    },
                );
            }
            Fact::EncodingMeasured {
                encoding,
                cutoff_hz,
                cliff_db,
                analyser,
            } => {
                // Last measurement wins, which is what a re-measure means.
                cat.measurements.insert(
                    *encoding,
                    Spectrum {
                        cutoff_hz: *cutoff_hz,
                        cliff_db: *cliff_db,
                        analyser: *analyser,
                    },
                );
            }
            Fact::RecordingRated { recording, stars } => {
                // Records arrive in stamp order, so the last simply wins.
                match stars {
                    Some(n) => {
                        cat.ratings.insert(of(recording), *n);
                    }
                    None => {
                        cat.ratings.remove(&of(recording));
                    }
                }
            }
            Fact::RecordingPlayed { recording, .. } => {
                *cat.plays.entry(of(recording)).or_insert(0) += 1;
                // The latest play, by the wall time the play happened, which
                // is what "days since played" is asking about. The stamp
                // orders the log; this answers the question.
                //
                // **In seconds**, like every other fact and like the filter
                // language's clock. A stamp is in milliseconds, and handing
                // that straight out made "played a minute ago" read as fifty
                // years from now.
                let seconds = stamp.wall / 1_000;
                let latest = cat.last_played.entry(of(recording)).or_insert(seconds);
                *latest = (*latest).max(seconds);
            }
            Fact::RecordingIdentified { recording, basis } => {
                // **No conflict rule, because there cannot be a conflict.**
                //
                // The id is `of_basis(basis)`, so two different bases reaching
                // one recording would be a UUIDv5 collision. Every fact naming
                // this id carries the same basis, and the only thing two
                // devices can disagree about is whether they have said it yet.
                //
                // A "best rung wins" comparison was written here first and is
                // gone: a mutation that reversed it failed no test, which is
                // the signature of a branch that cannot run. A file that later
                // gains a MusicBrainz id does not update this row — it mints a
                // *different* recording, and the two are joined by a merge.
                cat.identified
                    .entry(*recording)
                    .or_insert_with(|| basis.clone());
            }
            // Applied in the pass above, because a merge has to move what was
            // said before it.
            Fact::RecordingMerged { .. }
            | Fact::ReleaseGroupMerged { .. }
            | Fact::ArtistMerged { .. } => {}
            Fact::RecordingAcquired { recording, at } => {
                let first = cat.acquired.entry(of(recording)).or_insert(*at);
                *first = (*first).min(*at);
            }
        }
    }

    // **A placement onto a MusicBrainz release outranks one onto a name.**
    //
    // Last writer wins was the rule and it loses real information. A release
    // is minted from its MusicBrainz id where the tags carry one and from its
    // release group otherwise (`of_release`), and **identical audio is one
    // encoding however many files hold it** — so scanning a badly tagged copy
    // re-places the well tagged one.
    //
    // Measured: `Luna` is held under `~/MusicLibrary` with a MusicBrainz album
    // id and was held under `~/DanceMusic-staging` with seven tags and none.
    // Same audio, so the same 32 encodings — and importing the staging folder
    // moved all 32 onto a medium minted from the album *name*, off the
    // MusicBrainz-derived medium where their catalogued tracklist sits. The
    // tracklist then appeared to have attached to nothing. 39 files across the
    // library were sitting on the weaker medium when this was written.
    //
    // The rung is computable rather than recorded: a release equal to
    // `of_release(None, group)` was minted from the group, and anything else
    // came from an id. So no fact needs a new field to say which it was.
    //
    // Resolved after the loop because a placement can be folded before the
    // release it points at, and the rung is a property of that release.
    // **And who said so outranks both** — #170. Where two placements name
    // MusicBrainz-derived releases the computed rung cannot separate them, so
    // recency decides and a scan is always the most recent thing that ran.
    // `Source` is what breaks that tie: a pass that read a whole tracklist and
    // matched it against every file held says more than a tag does.
    for (encoding, tracks) in &claimed {
        let rung = |t: &Id<Track>| {
            cat.tracks
                .get(t)
                .and_then(|slot| cat.media.get(&slot.medium))
                .and_then(|m| cat.releases.get(&m.release).map(|r| (m.release, r.group)))
                .is_some_and(|(release, group)| release != Id::<Release>::of_release(None, group))
        };
        let strongest = tracks.iter().map(|(_, b)| *b).max();
        if let Some(best) = tracks
            .iter()
            .rev()
            .filter(|(_, b)| Some(*b) == strongest)
            .find(|(t, _)| rung(t))
            .or_else(|| tracks.iter().rev().find(|(_, b)| Some(*b) == strongest))
        {
            cat.placed.insert(*encoding, best.0);
        }
    }

    // **Retitles last, and that is a rung rather than an ordering trick.**
    //
    // Last writer wins is the rule for a descriptive fact, and it does not
    // work here: `query` and `apply` bring their view up to date by scanning
    // the disk, and a scan re-asserts `RecordingCatalogued` from the tags with
    // a fresh stamp. Worse, it asserts it against the *current* recording id —
    // so after a relink the fact is new rather than a repeat, `drop_repeats`
    // keeps it, and it lands on top of anything written earlier.
    //
    // Measured the hard way: 23 retitles applied cleanly and not one of them
    // was visible afterwards, because the next read re-scanned over them.
    //
    // So a retitle is curation and a scan is a reading, and curation outranks
    // a reading whenever it was written. That is the same relationship
    // `Source` encodes for a claim; `RecordingCatalogued` carries no rung, and
    // giving it one is a larger change than this needs.
    for Record { fact, .. } in records {
        match fact {
            Fact::RecordingRetitled { recording, title } => {
                if let Some(recorded) = cat.recordings.get_mut(&of(recording)) {
                    recorded.title = title.clone();
                }
            }
            Fact::ReleaseGroupRetitled { group, title } => {
                if let Some(album) = cat.groups.get_mut(&group_of(group)) {
                    album.title = title.clone();
                }
            }
            _ => {}
        }
    }

    for ((encoding, base), mut asserted) in bindings {
        asserted.sort();
        let distinct: BTreeSet<_> = asserted.iter().map(|(_, r)| *r).collect();
        let winner = asserted.last().expect("non-empty by construction").1;

        // **Recorded whether or not anyone has described these bytes.**
        //
        // This used to write the binding onto the `Encoded` row and drop it
        // when there was none, which made a relink invisible to anything that
        // had not also scanned. `ingest` checks exactly this before minting —
        // "a binding someone made wins over the one we would mint" — so
        // dropping it defeated the one guard stopping a scan from silently
        // undoing an identification. Applying 377 of them left the work list
        // where it was, and that is how it was found.
        //
        // A binding is a fact about an encoding *id*. Whether its bytes have
        // been described is a different fact and a different question.
        cat.bound.insert(encoding, winner);
        if let Some(d) = cat.encodings.get_mut(&encoding) {
            d.recording = Some(winner);
        }
        if distinct.len() > 1 {
            cat.conflicts.push(Conflict {
                encoding,
                base,
                asserted,
            });
        }
    }
    cat.conflicts.sort_by_key(|c| c.encoding);
    cat
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the genre curation backlog, which no command asks for"
)]
impl Catalogue {
    /// Live assertions about one subject, on one axis.
    ///
    /// **The reader's half of one map holding every facet** (#117). Callers
    /// used to reach for `genres` or `qualifiers` and get the axis for free;
    /// now they name it, which is the point — asking for "the genres" without
    /// saying so was what let a single-value row shape look reasonable.
    ///
    /// Retracted pairs are gone and node labels never appear: ISO 25964's rule
    /// is that a term put into an array to organise it does not take part in
    /// retrieval.
    pub fn asserted(
        &self,
        subject: Described,
        facet: Id<Facet>,
    ) -> Vec<(Id<Concept>, Source, Confidence)> {
        self.assertions
            .iter()
            .filter(|((s, c, _), _)| {
                *s == subject
                    && !self.assertions_retracted.contains_key(&(*s, *c))
                    && self
                        .concepts
                        .get(c)
                        .is_some_and(|r| r.facet == facet && !r.node_label)
            })
            .map(|((_, c, basis), confidence)| (*c, *basis, *confidence))
            .collect()
    }

    /// The same, as the labels a person reads, deduplicated and sorted.
    pub fn labels_on(&self, subject: Described, facet_key: &str) -> Vec<&str> {
        let facet = Id::<Facet>::of_key(facet_key);
        let mut out: Vec<&str> = self
            .asserted(subject, facet)
            .into_iter()
            .filter_map(|(c, _, _)| self.concepts.get(&c).map(|r| r.label.as_str()))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Concepts this hierarchy does not place — a standing review query.
    ///
    /// **An orphan concept is legitimate, not an error.** A genre defined at
    /// listen time, on the track that prompted it, has no reason to wait for
    /// somebody to decide where it sits: assignment does not wait for
    /// placement, and refusing the assignment until it does would mean the
    /// moment you know the answer is the moment you cannot record it.
    ///
    /// What it costs is reachability. `genre+ = Electronic` walks edges, so an
    /// unplaced concept is invisible to every closure query until an edge
    /// exists — found only by naming it exactly. That is why this is a query
    /// rather than a rule: the concepts are fine, and the list of them is the
    /// curation backlog.
    /// **The axis is an argument, not read off the hierarchy.** A hierarchy
    /// need not be declared before an edge names it — `narrows` mints the
    /// edge, not the array — so deriving the facet from
    /// [`Catalogue::hierarchies`] would silently return nothing for exactly
    /// the vocabularies still being built, which is when a backlog matters.
    pub fn unplaced_concepts(
        &self,
        hierarchy: Id<Hierarchy>,
        facet: Id<Facet>,
    ) -> BTreeSet<Id<Concept>> {
        let placed: BTreeSet<Id<Concept>> = self
            .narrows
            .iter()
            .filter(|(h, _, _)| *h == hierarchy)
            .flat_map(|(_, parent, child)| [*parent, *child])
            .collect();
        // **Only this hierarchy's own axis**, or every version concept would
        // read as an unplaced genre the moment one list held both (#117).
        self.concepts
            .iter()
            .filter(|(id, c)| c.facet == facet && !placed.contains(*id))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The merges the collisions are asking for.
    ///
    /// **Why an orphan is left rather than swept.** A recording that nothing
    /// holds is an ordinary row — §1 is explicit that a wishlist entry is
    /// exactly one of those, so "delete what nothing points at" would delete
    /// everything you have decided about and do not own. Structurally the two
    /// are identical.
    ///
    /// The model can still tell them apart, because a collision *names* its
    /// loser. So the answer is not to collect the orphan but to propose
    /// retiring it: one encoding cannot be two performances, so the pair is
    /// the same song twice and a redirect (§3) settles it without deleting
    /// anything. This is what feeds the staged queue in §3 — the machine finds
    /// them, a person decides.
    pub fn proposed_merges(&self) -> Vec<(Id<Recording>, Id<Recording>)> {
        let mut out: Vec<_> = self
            .conflicts
            .iter()
            .flat_map(|c| c.losers().map(move |l| (l, c.winner())))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every device that claims to hold this encoding, with where and when.
    ///
    /// Deliberately not "is it available": that needs to know which device is
    /// *this* one and which are reachable right now, neither of which is a fact
    /// about the library. #38 layers Local / Remote / Absent over this.
    pub fn holders(&self, encoding: Id<Encoding>) -> Vec<(Id<Device>, &str, Stamp)> {
        self.holdings
            .iter()
            .filter_map(|(device, a)| {
                a.held
                    .get(&encoding)
                    .map(|locator| (*device, locator.as_str(), a.at))
            })
            .collect()
    }
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — compaction is safe to run whenever and is run by nothing"
)]
/// Drop the holdings assertions nothing reads any more.
///
/// **The one event type that may be compacted.** A superseded assertion
/// carries nothing — the later one says everything it said and more — where a
/// superseded play still carries a listen and a superseded rating still carries
/// what someone once thought.
///
/// Without this the log grows by a full file listing every time a device scans.
/// Measured on the live library, scan observations were 6227 of 6572 events
/// after three days of use: 95% of the log, and none of it worth keeping.
///
/// Folding a compacted set gives exactly what folding the original gives, which
/// is the property that makes this safe to run whenever, and the one the tests
/// assert directly.
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

/// Records order by their stamp, which is total across devices — so a set of
/// records has one iteration order everywhere.
impl Ord for Record {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.stamp.cmp(&other.stamp)
    }
}

/// The catalogue answers the player's history questions directly.
///
/// One impl rather than a wrapper type, because the fold already *is* the
/// answer — plays counted, the latest rating decided, the earliest acquisition
/// taken. Anything in between would be a second place for these to be wrong.
/// Walk a derivation graph upward from one node, bounded — #79.
///
/// Breadth-first, so `depth` is the shortest way back to each ancestor, and
/// `seen` is what stops a cycle rather than the bound. **The bound is the
/// backstop**: edges are rows and rows union, so two devices can author a
/// cycle neither of them meant, and a render must answer rather than hang.
///
/// Returns `(kind, depth)` per edge taken, which is what makes the bracket
/// correlated: the kind and the depth come from the same hop.
fn climb<T: Ord + Copy, F>(start: T, parents: F) -> Vec<(String, i64)>
where
    F: Fn(T) -> Vec<(T, String)>,
{
    /// Sixteen, as everywhere else here: deeper than any real chain of
    /// remixes, shallower than a loop can hide in.
    const MAX_DEPTH: i64 = 16;

    let mut seen = std::collections::BTreeSet::from([start]);
    let mut here = vec![start];
    let mut out = Vec::new();
    for depth in 1..=MAX_DEPTH {
        let mut next = Vec::new();
        for node in here.drain(..) {
            for (parent, kind) in parents(node) {
                if seen.insert(parent) {
                    out.push((kind, depth));
                    next.push(parent);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        here = next;
    }
    out
}

impl crate::filter::Listened for Catalogue {
    fn plays(&self, recording: Id<Recording>) -> u32 {
        self.plays.get(&recording).copied().unwrap_or(0)
    }

    fn last_played(&self, recording: Id<Recording>) -> Option<i64> {
        self.last_played.get(&recording).copied()
    }

    fn acquired(&self, recording: Id<Recording>) -> Option<i64> {
        self.acquired.get(&recording).copied()
    }

    fn rating(&self, recording: Id<Recording>) -> Option<u8> {
        self.ratings.get(&recording).copied()
    }

    fn derived_from(&self, recording: Id<Recording>) -> Vec<(String, i64)> {
        climb(recording, |here| {
            self.derived
                .iter()
                .filter(move |e| e.1 == here && !self.derived_retracted.contains(e))
                .map(|(from, _, kind)| (*from, format!("{kind:?}").to_lowercase()))
                .collect()
        })
    }

    fn work_derived_from(&self, recording: Id<Recording>) -> Vec<(String, i64)> {
        // **Through the work the recording performs.** A row is a recording,
        // and the other traversal is one level up — so it is reached rather
        // than merged into the recording graph.
        let mut out = Vec::new();
        for work in self
            .performances
            .iter()
            .filter(|(r, _)| *r == recording)
            .map(|(_, w)| *w)
        {
            out.extend(climb(work, |here| {
                self.work_derived
                    .iter()
                    .filter(move |e| e.1 == here && !self.work_derived_retracted.contains(e))
                    .map(|(from, _, kind)| (*from, format!("{kind:?}").to_lowercase()))
                    .collect()
            }));
        }
        out
    }

    fn claims(&self, recording: Id<Recording>, facet_key: &str) -> Vec<(String, String, String)> {
        let facet = Id::<crate::model::entity::Facet>::of_key(facet_key);
        self.asserted(crate::model::facet::Described::Recording(recording), facet)
            .into_iter()
            .filter_map(|(concept, basis, confidence)| {
                let label = self.concepts.get(&concept)?.label.clone();
                Some((
                    label,
                    basis.written().to_string(),
                    confidence.written().to_string(),
                ))
            })
            .collect()
    }

    fn broader(&self, facet_key: &str, label: &str) -> Vec<String> {
        use crate::model::entity::Concept;
        use std::collections::BTreeSet;

        // How far a walk may climb before the data is called cyclic. Sixteen
        // is `vocabulary::MAX_DEPTH`, for the same reason: deeper than any
        // real music taxonomy, shallower than a loop can hide in.
        const MAX_DEPTH: usize = 16;

        let mut seen: BTreeSet<Id<Concept>> = BTreeSet::new();
        let mut here = vec![Id::<Concept>::in_facet(facet_key, label)];
        let mut out = Vec::new();
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for child in here.drain(..) {
                for (_, parent, _) in self.narrows.iter().filter(|(h, p, c)| {
                    *c == child && !self.narrows_retracted.contains(&(*h, *p, *c))
                }) {
                    // `seen` is what stops a cycle rather than the bound: the
                    // bound is the backstop for a chain long enough that
                    // walking it was a mistake either way.
                    if seen.insert(*parent) {
                        if let Some(record) = self.concepts.get(parent) {
                            out.push(record.label.clone());
                        }
                        next.push(*parent);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            here = next;
        }
        out
    }

    /// **Distinct collections, not entries** — a playlist holding one track
    /// twice is one collection that holds it, and counting entries would make
    /// `collected>1` mean "repeated somewhere" instead of "in two crates".
    fn collected(&self, recording: Id<Recording>) -> u32 {
        self.entries
            .iter()
            .filter(|(_, _, r)| *r == recording)
            .map(|(c, _, _)| *c)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u32
    }
}

#[cfg(test)]
mod closure_tests {
    use super::*;
    use crate::filter::Listened;
    use crate::model::entity::{Device, Hierarchy};
    use crate::model::instance::Instance;

    fn built(edges: &[(&str, &str)]) -> Catalogue {
        let mut i = Instance::new(Id::<Device>::derived(b"test"));
        let h = Id::<Hierarchy>::derived(b"browse");
        for (parent, child) in edges {
            for label in [parent, child] {
                i.assert_concept(
                    100,
                    crate::model::facet::Described::Recording(Id::derived(b"anything")),
                    crate::model::facet::GENRE,
                    label,
                    crate::model::reference::Source::Manual,
                    crate::model::ingest::Confidence::Confirmed,
                );
            }
            i.narrows(100, h, crate::model::facet::GENRE, parent, child);
        }
        i.catalogue()
    }

    /// **The derivation walk is bounded and cycle-safe** — #79, and the one
    /// thing `domain-model.md` §5 says is not optional.
    ///
    /// Edges are rows and rows union, so one device saying `A derives B` while
    /// another says `B derives A` produces a cycle neither authored. An
    /// unbounded walk hangs the render; this answers.
    #[test]
    fn a_derivation_cycle_answers_rather_than_hanging() {
        use crate::filter::Listened;
        use crate::model::spine::RecordingDerivationKind::Remix;

        let song = |n: u8| Id::<Recording>::derived(&[n]);
        let mut i = crate::model::instance::Instance::new(Id::derived(b"laptop"));
        // A chain that closes on itself: 1 → 2 → 3 → 1.
        i.derives(100, song(1), song(2), Remix);
        i.derives(100, song(2), song(3), Remix);
        i.derives(100, song(3), song(1), Remix);

        let cat = i.catalogue();
        let up = cat.derived_from(song(3));
        assert_eq!(
            up.len(),
            2,
            "each ancestor is reached once, and the loop did not repeat them: {up:?}"
        );
        assert!(up.iter().all(|(_, depth)| *depth <= 16), "{up:?}");
    }

    /// **The depth is the shortest way back**, and a retraction takes an edge
    /// out of the walk without deleting the fact.
    #[test]
    fn a_derivation_walk_reports_hops_and_respects_a_retraction() {
        use crate::filter::Listened;
        use crate::model::spine::RecordingDerivationKind::{Edit, Remix};

        let song = |n: u8| Id::<Recording>::derived(&[n]);
        let mut i = crate::model::instance::Instance::new(Id::derived(b"laptop"));
        // 1 is the original, 2 is a remix of it, 3 is an edit of the remix.
        i.derives(100, song(1), song(2), Remix);
        i.derives(100, song(2), song(3), Edit);

        let cat = i.catalogue();
        let mut up = cat.derived_from(song(3));
        up.sort();
        assert_eq!(up, [("edit".to_string(), 1), ("remix".to_string(), 2)]);

        i.retract_derivation(200, song(1), song(2), Remix);
        assert_eq!(
            i.catalogue().derived_from(song(3)),
            [("edit".to_string(), 1)],
            "a retracted edge was still walked"
        );
    }

    /// **Closure walks up, at any depth** — #93.
    #[test]
    fn broader_reaches_every_ancestor() {
        let cat = built(&[("Music", "Electronic"), ("Electronic", "Techno")]);
        let up = cat.broader(crate::model::facet::GENRE, "Techno");
        assert!(up.contains(&"Electronic".to_string()), "{up:?}");
        assert!(
            up.contains(&"Music".to_string()),
            "not at any depth: {up:?}"
        );
        assert!(
            !up.contains(&"Techno".to_string()),
            "a concept is not broader than itself"
        );
    }

    /// **A cycle must not hang a render** — #79's requirement, and the reason
    /// the bound is not optional.
    ///
    /// A hierarchy is curated by hand, so a loop is a question of when rather
    /// than whether: `Techno` narrower-of `Rock` narrower-of `Techno` is two
    /// edits nobody would make together and somebody eventually will.
    #[test]
    fn a_cycle_in_the_hierarchy_terminates() {
        let cat = built(&[("Rock", "Techno"), ("Techno", "Rock")]);
        let up = cat.broader(crate::model::facet::GENRE, "Techno");
        assert!(up.contains(&"Rock".to_string()), "{up:?}");
        // It returns, which is the whole assertion — and each concept once.
        assert_eq!(up.len(), 2, "walked a loop more than once: {up:?}");
    }

    /// A withdrawn edge stops being walked, or repairing a wrong placement
    /// would mean the hierarchy could only ever grow.
    #[test]
    fn a_retracted_edge_is_not_climbed() {
        let mut i = Instance::new(Id::<Device>::derived(b"test"));
        let h = Id::<Hierarchy>::derived(b"browse");
        for label in ["Rock", "Techno"] {
            i.assert_concept(
                100,
                crate::model::facet::Described::Recording(Id::derived(b"anything")),
                crate::model::facet::GENRE,
                label,
                crate::model::reference::Source::Manual,
                crate::model::ingest::Confidence::Confirmed,
            );
        }
        i.narrows(100, h, crate::model::facet::GENRE, "Rock", "Techno");
        assert_eq!(
            i.catalogue().broader(crate::model::facet::GENRE, "Techno"),
            ["Rock"]
        );

        i.retract_narrowing(200, h, crate::model::facet::GENRE, "Rock", "Techno");
        assert!(
            i.catalogue()
                .broader(crate::model::facet::GENRE, "Techno")
                .is_empty(),
            "a withdrawn edge was still climbed"
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::model::clock::HybridClock;
    use crate::model::entity::Device;

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn recording(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// **The earliest wins, whatever order it is heard in.** Min is
    /// commutative, associative and idempotent, so two devices converge on the
    /// acquisition date with no rule to apply and nothing to surface.
    #[test]
    fn acquired_folds_to_the_earliest_anyone_says() {
        let rec = recording(1);
        let mut clock = HybridClock::new(device(1));
        // Deliberately out of order: the later claim is stamped first.
        let records: BTreeSet<Record> = [
            Record {
                stamp: clock.stamp(1_000),
                fact: Fact::RecordingAcquired {
                    recording: rec,
                    at: 9_999,
                },
            },
            Record {
                stamp: clock.stamp(2_000),
                fact: Fact::RecordingAcquired {
                    recording: rec,
                    at: 500,
                },
            },
            Record {
                stamp: clock.stamp(3_000),
                fact: Fact::RecordingAcquired {
                    recording: rec,
                    at: 7_000,
                },
            },
        ]
        .into();
        assert_eq!(
            fold(&records).acquired.get(&rec).copied(),
            Some(500),
            "the earliest, not the latest and not the last heard"
        );
    }

    /// `days_since_played` asks when the play happened, so the fold keeps the
    /// most recent one rather than the count alone.
    #[test]
    fn last_played_is_the_most_recent_play() {
        let rec = recording(1);
        let mut clock = HybridClock::new(device(1));
        // Stamped in milliseconds, which is the clock's unit.
        let records: BTreeSet<Record> = [5_000_000i64, 9_000_000, 7_000_000]
            .into_iter()
            .map(|wall| Record {
                stamp: clock.stamp(wall),
                fact: Fact::RecordingPlayed {
                    recording: rec,
                    encoding: None,
                },
            })
            .collect();
        let cat = fold(&records);
        assert_eq!(cat.plays.get(&rec).copied(), Some(3), "all three counted");
        // Read back in **seconds**, like every other date the player compares.
        // Handing out the stamp's milliseconds made "played a minute ago" read
        // as fifty years from now, and only running the thing showed it.
        assert_eq!(
            cat.last_played.get(&rec).copied(),
            Some(9_000),
            "the newest play, and in seconds"
        );
    }

    /// A recording nobody has played or acquired has neither, which is what
    /// makes `!play_count?` and `!days_since_added?` findable.
    #[test]
    fn a_silent_recording_has_no_history() {
        let cat = fold(&BTreeSet::new());
        assert!(cat.acquired.is_empty());
        assert!(cat.last_played.is_empty());
    }
}

#[cfg(test)]
mod merge_tests {
    //! **A merge moves what was said, or it is a delete with extra steps.**

    use super::*;
    use crate::model::basis::Basis;
    use crate::model::clock::HybridClock;

    fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn log(facts: impl IntoIterator<Item = Fact>) -> BTreeSet<Record> {
        let mut clock = HybridClock::new(Id::derived(b"laptop"));
        facts
            .into_iter()
            .map(|fact| Record {
                stamp: clock.stamp(1_000),
                fact,
            })
            .collect()
    }

    /// **The case that actually happened**, in miniature: a rating and two
    /// plays given before anyone knew what the recording was.
    #[test]
    fn history_given_to_the_loser_lands_on_the_winner() {
        let (old, new) = (rec(1), rec(2));
        let cat = fold(&log([
            Fact::RecordingRated {
                recording: old,
                stars: Some(5),
            },
            Fact::RecordingPlayed {
                recording: old,
                encoding: Some(Id::derived(b"e")),
            },
            Fact::RecordingPlayed {
                recording: new,
                encoding: Some(Id::derived(b"e")),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
        ]));

        assert_eq!(cat.ratings.get(&new), Some(&5), "the rating was stranded");
        assert_eq!(cat.plays.get(&new), Some(&2), "the plays did not add up");
        assert!(!cat.ratings.contains_key(&old) && !cat.plays.contains_key(&old));
    }

    /// **A merge recorded on Tuesday moves a rating given on Monday**, which
    /// is why merges are folded in a pass of their own. Applied as they are
    /// reached, everything earlier stays where it was.
    #[test]
    fn a_merge_reaches_backwards() {
        let (old, new) = (rec(1), rec(2));
        let cat = fold(&log([
            Fact::RecordingRated {
                recording: old,
                stars: Some(4),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
        ]));
        assert_eq!(cat.ratings.get(&new), Some(&4));
    }

    /// The row goes too, or the library shows the song twice.
    #[test]
    fn a_merged_recording_keeps_no_row_of_its_own() {
        let (old, new) = (rec(1), rec(2));
        let cat = fold(&log([
            Fact::RecordingCatalogued {
                recording: old,
                title: "0011".into(),
                artist: Id::derived(b"brinkmann"),
            },
            Fact::RecordingIdentified {
                recording: new,
                basis: Basis::MusicBrainz(uuid::Uuid::from_u128(9)),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
        ]));
        assert!(
            !cat.recordings.contains_key(&old),
            "the song is listed twice"
        );
        assert_eq!(cat.recordings[&new].title, "0011");
    }

    /// **A badly tagged copy does not re-place a well tagged one.**
    ///
    /// Identical audio is one encoding however many files hold it, and
    /// `EncodingPlaced` was last-writer-wins — so scanning the worse copy moved
    /// the better one. Measured: `Luna` was held under `~/MusicLibrary` with a
    /// MusicBrainz album id and under `~/DanceMusic-staging` with seven tags
    /// and none. Importing the staging folder moved all 32 encodings onto a
    /// medium minted from the album *name*, and the tracklist catalogued
    /// against the MusicBrainz medium then appeared to have attached to
    /// nothing. 39 files were sitting on the weaker medium.
    ///
    /// The rung is computable: a release equal to `of_release(None, group)`
    /// came from the group, anything else came from an id.
    #[test]
    fn a_placement_from_a_musicbrainz_release_outranks_one_from_a_name() {
        use crate::model::entity::{Artist, Medium, Release, ReleaseGroup, Track};
        let group = Id::<ReleaseGroup>::derived(b"luna");
        let named = Id::<Release>::of_release(None, group);
        let minted = Id::<Release>::of_musicbrainz(uuid::Uuid::from_u128(0xdc70));
        let good = Id::<Medium>::of_medium(minted, 1);
        let weak = Id::<Medium>::of_medium(named, 1);
        let song = Id::<Recording>::derived(b"triton");
        let copy = Id::<Encoding>::derived(b"one rip");
        let here = Id::<Track>::of_track(good, song);
        let there = Id::<Track>::of_track(weak, song);
        let disc = |release, medium| Fact::MediumCatalogued {
            medium,
            release,
            position: 1,
            format: None,
            tracks: Some(34),
        };
        let pressing = |release| Fact::ReleaseCatalogued {
            release,
            group,
            title: "Luna".into(),
            released: None,
            label: None,
            media: Some(1),
        };
        let slot = |track, medium| Fact::TrackPlaced {
            track,
            medium,
            recording: song,
            position: 1,
            title_on_release: None,
        };
        let cat = fold(&log([
            pressing(minted),
            pressing(named),
            disc(minted, good),
            disc(named, weak),
            slot(here, good),
            slot(there, weak),
            Fact::RecordingCatalogued {
                recording: song,
                title: "Triton".into(),
                artist: Id::<Artist>::derived(b"bodzin"),
            },
            // The well tagged copy first, then the badly tagged one — which is
            // the order that lost the good placement.
            Fact::EncodingPlaced {
                encoding: copy,
                track: here,
                basis: crate::model::reference::Source::TagVariant,
            },
            Fact::EncodingPlaced {
                encoding: copy,
                track: there,
                basis: crate::model::reference::Source::TagVariant,
            },
        ]));
        assert_eq!(
            cat.placed.get(&copy),
            Some(&here),
            "the later, worse-evidenced placement won"
        );
    }

    /// **And where nothing is better evidenced, the last word still wins.**
    /// The rung picks the best available; it does not freeze a placement.
    #[test]
    fn among_placements_of_one_rung_the_latest_stands() {
        use crate::model::entity::{Medium, Release, ReleaseGroup, Track};
        let group = Id::<ReleaseGroup>::derived(b"a record");
        let named = Id::<Release>::of_release(None, group);
        let one = Id::<Medium>::of_medium(named, 1);
        let two = Id::<Medium>::of_medium(named, 2);
        let song = Id::<Recording>::derived(b"a song");
        let copy = Id::<Encoding>::derived(b"a copy");
        let first = Id::<Track>::of_track(one, song);
        let last = Id::<Track>::of_track(two, song);
        let cat = fold(&log([
            Fact::ReleaseCatalogued {
                release: named,
                group,
                title: "A Record".into(),
                released: None,
                label: None,
                media: Some(2),
            },
            Fact::EncodingPlaced {
                encoding: copy,
                track: first,
                basis: crate::model::reference::Source::TagVariant,
            },
            Fact::EncodingPlaced {
                encoding: copy,
                track: last,
                basis: crate::model::reference::Source::TagVariant,
            },
        ]));
        assert_eq!(cat.placed.get(&copy), Some(&last));
    }

    /// **A retraction shadows a credit, and a later assertion lifts it.**
    ///
    /// The log is append-only and merges by union, so a credit is live or not
    /// according to whichever of assert and retract carries the later stamp —
    /// the same rule `ConceptRetracted` follows, and for the same reason: a
    /// stale replica re-delivering the assertion must not resurrect it.
    #[test]
    fn a_withdrawn_credit_stays_withdrawn_until_somebody_asserts_it_again() {
        use crate::model::entity::{Artist, ReleaseGroup, Role};
        let album = Id::<ReleaseGroup>::derived(b"luna");
        let joined = Id::<Artist>::derived(b"stephan bodzin & marc romboy");
        let performer = Id::<Role>::derived(b"performer");
        let on = CreditOn::ReleaseGroup(album);
        let assert = || Fact::CreditAsserted {
            subject: on,
            artist: joined,
            role: performer,
            basis: Source::TagVariant,
            position: 0,
            as_credited: None,
            join_phrase: None,
        };
        let withdraw = || Fact::CreditRetracted {
            subject: on,
            artist: joined,
            role: performer,
            basis: Source::Manual,
        };

        let live = fold(&log([assert()]));
        assert_eq!(live.credits.len(), 1);

        let gone = fold(&log([assert(), withdraw()]));
        assert!(
            gone.credits.is_empty(),
            "the credit survived its retraction"
        );
        assert_eq!(
            gone.credits_retracted.get(&(on, joined, performer)),
            Some(&Source::Manual),
            "who ruled it out was not recorded"
        );

        let again = fold(&log([assert(), withdraw(), assert()]));
        assert_eq!(again.credits.len(), 1, "a later assertion did not lift it");
        assert!(again.credits_retracted.is_empty());
    }

    /// **It shadows every basis at once**, being keyed one field shorter than
    /// the assertion. Somebody saying this artist is not on this record is
    /// answering the question, not arguing with one asserter.
    #[test]
    fn a_withdrawal_does_not_leave_the_same_credit_standing_on_another_rung() {
        use crate::model::entity::{Artist, ReleaseGroup, Role};
        let album = Id::<ReleaseGroup>::derived(b"luna");
        let joined = Id::<Artist>::derived(b"a joined credit");
        let performer = Id::<Role>::derived(b"performer");
        let on = CreditOn::ReleaseGroup(album);
        let assert = |basis| Fact::CreditAsserted {
            subject: on,
            artist: joined,
            role: performer,
            basis,
            position: 0,
            as_credited: None,
            join_phrase: None,
        };
        let cat = fold(&log([
            assert(Source::TagVariant),
            assert(Source::MusicBrainz),
            Fact::CreditRetracted {
                subject: on,
                artist: joined,
                role: performer,
                basis: Source::Manual,
            },
        ]));
        assert!(
            cat.credits.is_empty(),
            "a credit asserted twice needed retracting twice"
        );
    }

    /// **A retitle changes the label and nothing else** — the fact an
    /// identification could not write.
    ///
    /// The case that wanted it: two James Blake tracks in one folder with
    /// their tags transposed. Identifying them bound each encoding to the
    /// right performance and merged the byte-minted rows away — and left both
    /// titles wrong, because a merge carries the loser's description onto the
    /// winner and nothing wrote a new one.
    #[test]
    fn a_retitle_moves_the_label_and_leaves_the_credit() {
        use crate::model::entity::Artist;
        let song = Id::<Recording>::derived(b"give a man a rod");
        let blake = Id::<Artist>::derived(b"james blake");
        let cat = fold(&log([
            Fact::RecordingCatalogued {
                recording: song,
                title: "The Bells Sketch".into(),
                artist: blake,
            },
            Fact::RecordingRetitled {
                recording: song,
                title: "Give a Man a Rod".into(),
            },
        ]));
        assert_eq!(cat.recordings[&song].title, "Give a Man a Rod");
        assert_eq!(
            cat.recordings[&song].artist, blake,
            "a retitle is not a recredit"
        );
    }

    /// **An album can be told what it is called, and a merge is why.**
    ///
    /// A merge unites what both sides said and a title is descriptive, so the
    /// last writer takes it. Three editions of *21* are one release group
    /// holding three releases — the shape `Barton Hollow` settled (#134) — and
    /// merging them named the album `21 (Target Bonus Disc)`.
    #[test]
    fn an_album_can_be_retitled_and_the_retitle_follows_a_merge() {
        use crate::model::entity::{Artist, ReleaseGroup};
        let standard = Id::<ReleaseGroup>::derived(b"21");
        let target = Id::<ReleaseGroup>::derived(b"21 target bonus disc");
        let adele = Id::<Artist>::derived(b"adele");
        let catalogued = |group, title: &str| Fact::ReleaseGroupCatalogued {
            group,
            title: title.into(),
            artist: adele,
            first_released: None,
            kind: Default::default(),
            secondary: Default::default(),
        };
        let cat = fold(&log([
            catalogued(standard, "21"),
            catalogued(target, "21 (Target Bonus Disc)"),
            // The merge is right and hands the name to whichever was written
            // last, which is the whole problem.
            Fact::ReleaseGroupMerged {
                loser: target,
                winner: standard,
            },
            // Named against the loser, because that is the id a person reading
            // the merge has in front of them. Redirects resolve on read.
            Fact::ReleaseGroupRetitled {
                group: target,
                title: "21".into(),
            },
        ]));
        assert!(
            !cat.groups.contains_key(&target),
            "the album is listed twice"
        );
        assert_eq!(cat.groups[&standard].title, "21");
        assert_eq!(
            cat.groups[&standard].artist, adele,
            "a retitle is not a recredit"
        );
    }

    /// **And it outlasts a scan**, for the reason a recording's does: every
    /// read walks the disk and re-asserts `ReleaseGroupCatalogued` from the
    /// tags, so last-writer-wins would hand the name back to the tag forever.
    #[test]
    fn an_album_retitle_is_not_undone_by_the_next_scan() {
        use crate::model::entity::{Artist, ReleaseGroup};
        let album = Id::<ReleaseGroup>::derived(b"21");
        let catalogued = |title: &str| Fact::ReleaseGroupCatalogued {
            group: album,
            title: title.into(),
            artist: Id::<Artist>::derived(b"adele"),
            first_released: None,
            kind: Default::default(),
            secondary: Default::default(),
        };
        let mut records = log([
            catalogued("21 (Target Bonus Disc)"),
            Fact::ReleaseGroupRetitled {
                group: album,
                title: "21".into(),
            },
        ]);
        // The next read, saying what the tags still say — and newer.
        records.extend(log([catalogued("21 (Target Bonus Disc)")]));
        assert_eq!(
            fold(&records).groups[&album].title,
            "21",
            "a scan wrote over a title somebody curated"
        );
    }

    /// **A retitle outlasts a scan, whenever it was written.**
    ///
    /// The bug this exists for: 23 retitles applied cleanly and not one was
    /// visible afterwards. `query` and `apply` bring their view up to date by
    /// scanning the disk, and a scan re-asserts `RecordingCatalogued` from the
    /// tags with a *fresh* stamp — so last-writer-wins hands the tag the win
    /// on every read, forever.
    ///
    /// Note the stamps here: the scan's fact is newer, and the retitle still
    /// wins. That is the point.
    #[test]
    fn a_retitle_is_not_undone_by_the_next_scan() {
        use crate::model::entity::Artist;
        let song = Id::<Recording>::derived(b"give a man a rod");
        let blake = Id::<Artist>::derived(b"james blake");
        let mut records = log([
            Fact::RecordingCatalogued {
                recording: song,
                title: "The Bells Sketch".into(),
                artist: blake,
            },
            Fact::RecordingRetitled {
                recording: song,
                title: "Give a Man a Rod".into(),
            },
        ]);
        // The next read walks the disk and says what the tags still say.
        records.extend(log([Fact::RecordingCatalogued {
            recording: song,
            title: "The Bells Sketch".into(),
            artist: blake,
        }]));

        assert_eq!(
            fold(&records).recordings[&song].title,
            "Give a Man a Rod",
            "a scan wrote over a title somebody curated"
        );
    }

    /// **And it follows a merge**, which is the shape it actually arrives in:
    /// an identification merges the byte-minted row into the MusicBrainz one,
    /// and the retitle names whichever of the two the tool was looking at.
    #[test]
    fn a_retitle_reaches_the_row_a_merge_moved_it_to() {
        use crate::model::entity::Artist;
        let old = Id::<Recording>::derived(b"minted from bytes");
        let new = Id::<Recording>::derived(b"named by musicbrainz");
        let cat = fold(&log([
            Fact::RecordingCatalogued {
                recording: old,
                title: "The Bells Sketch".into(),
                artist: Id::<Artist>::derived(b"james blake"),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
            Fact::RecordingRetitled {
                recording: old,
                title: "Give a Man a Rod".into(),
            },
        ]));
        assert!(!cat.recordings.contains_key(&old));
        assert_eq!(
            cat.recordings[&new].title, "Give a Man a Rod",
            "the retitle did not follow the merge"
        );
    }

    /// **A retitle of a row nothing catalogued does nothing.** It describes
    /// something held; `parse` refuses one whose subject is unknown, and this
    /// is the fold half of that — a stray fact must not mint a row with a
    /// title and no artist.
    #[test]
    fn a_retitle_does_not_invent_a_recording() {
        let nobody = Id::<Recording>::derived(b"never catalogued");
        let cat = fold(&log([Fact::RecordingRetitled {
            recording: nobody,
            title: "Out of nowhere".into(),
        }]));
        assert!(cat.recordings.is_empty(), "a title minted a row");
    }

    /// **An artist merge collapses a split credit into one row.**
    ///
    /// The measured case: of 1015 named artists this library holds, 82 names
    /// are held by more than one id, because `of_artist` mints from a
    /// MusicBrainz id where the tags carry one and from the name otherwise.
    /// `line_of` dedupes by rendered name, so both halves drew as one artist
    /// and the division was invisible — but the credits really were two rows,
    /// and a browse by artist really did split the work.
    #[test]
    fn a_merged_artist_takes_the_credits_given_to_both() {
        use crate::model::entity::Artist;
        let named = Id::<Artist>::derived(b"name\x1fumfang");
        let minted = Id::<Artist>::derived(b"musicbrainz\x1fumfang");
        let one = Id::<Recording>::derived(b"shant");
        let two = Id::<Recording>::derived(b"six");
        let performer = Id::<crate::model::entity::Role>::derived(b"performer");
        let credit = |recording, artist| Fact::CreditAsserted {
            subject: CreditOn::Recording(recording),
            artist,
            role: performer,
            basis: Source::TagVariant,
            position: 0,
            as_credited: None,
            join_phrase: None,
        };
        let cat = fold(&log([
            Fact::ArtistNamed {
                artist: named,
                name: "UMFANG".into(),
            },
            Fact::ArtistNamed {
                artist: minted,
                name: "Umfang".into(),
            },
            credit(one, named),
            credit(two, minted),
            Fact::ArtistMerged {
                loser: named,
                winner: minted,
            },
        ]));

        assert!(
            !cat.artist_names.contains_key(&named),
            "the artist is still listed twice"
        );
        assert_eq!(
            cat.credits.len(),
            2,
            "both credits survive — a merge joins the artist, not the work"
        );
        assert!(
            cat.credits.keys().all(|(_, artist, ..)| *artist == minted),
            "a credit did not follow the artist it was given to"
        );
        // Last writer wins, as it does for every descriptive fact: the merge
        // moves the loser's label onto the winner and the later one stands.
        assert_eq!(cat.artist_names[&minted], "Umfang");
    }

    /// **And it moves the album credit**, which is the one that reaches a
    /// folder: `file_under` asks the release group for its artist first, so a
    /// split left here files the two halves of one record apart. Three of the
    /// 82 splits differ in case as well — `Umfang` against `UMFANG` — and
    /// those are the ones that showed, as a folder named `Umfang, UMFANG`.
    #[test]
    fn a_merged_artist_takes_the_album_with_them() {
        use crate::model::entity::{Artist, ReleaseGroup};
        let named = Id::<Artist>::derived(b"name\x1fantigone");
        let minted = Id::<Artist>::derived(b"musicbrainz\x1fantigone");
        let group = Id::<ReleaseGroup>::derived(b"as i walk to you");
        let cat = fold(&log([
            Fact::ReleaseGroupCatalogued {
                group,
                title: "As I Walk to You".into(),
                artist: named,
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
            Fact::ArtistMerged {
                loser: named,
                winner: minted,
            },
        ]));
        assert_eq!(cat.groups[&group].artist, minted);
    }

    /// **An album merge moves the pressings and keeps them apart** — #156.
    ///
    /// The case that wanted this: *Barton Hollow* is a standard edition of 14
    /// tracks and a deluxe with 4 more, and a retag made them two albums. The
    /// answer is one album holding two releases — so the merge has to move the
    /// pressing's `group` and must not touch the pressing itself.
    #[test]
    fn a_merged_album_keeps_its_pressings_and_loses_its_row() {
        use crate::model::entity::{Release, ReleaseGroup};
        let old = Id::<ReleaseGroup>::derived(b"barton hollow, deluxe");
        let new = Id::<ReleaseGroup>::derived(b"barton hollow");
        let standard = Id::<Release>::derived(b"standard");
        let deluxe = Id::<Release>::derived(b"deluxe");
        let cat = fold(&log([
            Fact::ReleaseGroupCatalogued {
                group: new,
                title: "Barton Hollow".into(),
                artist: Id::derived(b"the civil wars"),
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
            Fact::ReleaseGroupCatalogued {
                group: old,
                title: "Barton Hollow".into(),
                artist: Id::derived(b"the civil wars"),
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
            Fact::ReleaseCatalogued {
                release: standard,
                group: new,
                title: "Barton Hollow".into(),
                released: None,
                label: None,
                media: Some(1),
            },
            Fact::ReleaseCatalogued {
                release: deluxe,
                group: old,
                title: "Barton Hollow".into(),
                released: None,
                label: None,
                media: Some(1),
            },
            Fact::ReleaseGroupMerged {
                loser: old,
                winner: new,
            },
        ]));

        assert!(!cat.groups.contains_key(&old), "the album is listed twice");
        assert_eq!(cat.releases.len(), 2, "**both pressings survive**");
        assert!(
            cat.releases.values().all(|r| r.group == new),
            "a pressing did not follow its album"
        );
    }

    /// A credit and a genre asserted on the loser are about the album, so they
    /// come too. Otherwise a merge files the two halves of one record under
    /// different artists.
    #[test]
    fn what_was_said_about_the_loser_is_said_about_the_winner() {
        use crate::model::entity::ReleaseGroup;
        use crate::model::facet::Described;
        use crate::model::reference::{CreditOn, RoleClass, Source};
        let old = Id::<ReleaseGroup>::derived(b"loser");
        let new = Id::<ReleaseGroup>::derived(b"winner");
        let who = Id::derived(b"the civil wars");
        let role = Id::<crate::model::entity::Role>::of_name("performer");
        let cat = fold(&log([
            Fact::RoleDefined {
                role,
                name: "performer".into(),
                class: RoleClass::Performer,
            },
            Fact::CreditAsserted {
                subject: CreditOn::ReleaseGroup(old),
                artist: who,
                role,
                basis: Source::TagVariant,
                position: 0,
                as_credited: None,
                join_phrase: None,
            },
            Fact::ReleaseGroupMerged {
                loser: old,
                winner: new,
            },
        ]));
        assert!(
            cat.credits
                .keys()
                .all(|(subject, ..)| *subject == CreditOn::ReleaseGroup(new)),
            "the album credit stayed on the row nothing points at"
        );
        let _ = Described::ReleaseGroup(new);
    }

    /// **A retired copy stops being one, and nothing else changes** — #159.
    ///
    /// The tombstone `storage-layout.md` §6 specified and nothing wrote. What
    /// it has to do is narrow: the library stops offering the *copy*, and the
    /// recording, its slot and everything said about it stay exactly where
    /// they are — because a row the library holds no copy of is ordinary, and
    /// since #155 it is a row on purpose.
    #[test]
    fn a_retired_copy_is_shadowed_and_its_row_is_not() {
        use crate::model::encoding::Format;
        let enc = Id::<crate::model::entity::Encoding>::derived(b"a copy");
        let audio = crate::audio_id::AudioId { scheme: 1, hash: 7 };
        let described = Fact::EncodingDescribed {
            encoding: enc,
            audio,
            format: Some(Format::Mp3),
            bitrate: 320,
            length_ms: 200_000,
            sample_rate: Some(44_100),
            channels: Some(2),
        };
        let cat = fold(&log([
            described.clone(),
            Fact::EncodingRetired { encoding: enc },
        ]));
        assert!(cat.retired.contains(&enc));
        assert!(
            cat.encodings.contains_key(&enc),
            "**nothing is destroyed** — the row is what makes this reversible"
        );

        // And back, because a step you cannot take back is one nobody takes.
        let cat = fold(&log([
            described,
            Fact::EncodingRetired { encoding: enc },
            Fact::EncodingRestored { encoding: enc },
        ]));
        assert!(
            !cat.retired.contains(&enc),
            "a restored copy is still shadowed"
        );
    }

    /// Nothing is destroyed. The loser's facts are all still in the log, which
    /// is what makes a merge undoable and a false one repairable.
    #[test]
    fn a_merge_destroys_nothing() {
        let records = log([
            Fact::RecordingRated {
                recording: rec(1),
                stars: Some(5),
            },
            Fact::RecordingMerged {
                loser: rec(1),
                winner: rec(2),
            },
        ]);
        assert!(records.iter().any(|r| matches!(
            &r.fact,
            Fact::RecordingRated { recording, .. } if *recording == rec(1)
        )));
    }
}

#[cfg(test)]
mod reachability {
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
        let source = include_str!("log.rs");
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
