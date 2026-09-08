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
use crate::model::reference::{AliasKind, Cataloguable, CreditOn, RoleClass, Source};
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
    pub measurements: BTreeMap<Id<Encoding>, (u32, u16)>,
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
    /// Every composition anyone has named.
    pub works: BTreeMap<Id<Work>, String>,
    /// Which recordings perform which works. Row existence: a recording can
    /// perform two, which is what a medley is.
    pub performances: BTreeSet<(Id<Recording>, Id<Work>)>,
    /// The role codelist.
    pub roles: BTreeMap<Id<Role>, (String, RoleClass)>,
    /// Every credit. Keyed by the claim, valued by how it was printed.
    pub credits: BTreeMap<(CreditOn, Id<Artist>, Id<Role>, Source), Credited>,
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
            _ => {}
        }
    }
    let of = |r: &Id<Recording>| cat.merges.resolve(*r);
    let group_of = |g: &Id<ReleaseGroup>| cat.group_merges.resolve(*g);
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
                cat.artist_names.insert(*artist, name.clone());
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
                        artist: *artist,
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
                        artist: *artist,
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
                cat.credits.insert(
                    (credit_on(subject), *artist, *role, *basis),
                    Credited {
                        position: *position,
                        as_credited: as_credited.clone(),
                        join_phrase: join_phrase.clone(),
                    },
                );
            }
            Fact::EncodingPlaced { encoding, track } => {
                cat.placed.insert(*encoding, *track);
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
                analyser,
            } => {
                // Last measurement wins, which is what a re-measure means.
                cat.measurements.insert(*encoding, (*cutoff_hz, *analyser));
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
            Fact::RecordingMerged { .. } | Fact::ReleaseGroupMerged { .. } => {}
            Fact::RecordingAcquired { recording, at } => {
                let first = cat.acquired.entry(of(recording)).or_insert(*at);
                *first = (*first).min(*at);
            }
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
