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
    Artist, Device, Encoding, Genre, Hierarchy, Medium, Qualifier, Recording, Release,
    ReleaseGroup, Role, Track, Work,
};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::reference::{AliasKind, Cataloguable, CreditOn, RoleClass, Source};
use crate::model::spine::PartialDate;
use crate::model::vocabulary::GenreOn;

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
    },

    /// Which recording an encoding holds. **Structural** — §4 Type 2, so it
    /// carries what it replaced and a disagreement is surfaced.
    EncodingBound {
        encoding: Id<Encoding>,
        from: Option<Id<Recording>>,
        to: Id<Recording>,
    },

    /// A genre on a recording. Membership is row existence, so this unions.
    /// A concept exists in the vocabulary, and what it is called.
    ///
    /// Needed before an assignment can name one by id — a device folding a
    /// log has to be able to resolve `Id<Genre>` to something a person reads.
    GenreCatalogued { genre: Id<Genre>, name: String },

    /// One grouping of the concepts exists. A vocabulary may carry several —
    /// a browse hierarchy and a filter hierarchy over one list, as in SDMX.
    HierarchyDefined {
        hierarchy: Id<Hierarchy>,
        name: String,
    },

    /// One narrower-of edge, in one hierarchy.
    ///
    /// Row existence, so it unions across devices and there is no
    /// `Genre.children` for two curators to collide over.
    HierarchyNarrowed {
        hierarchy: Id<Hierarchy>,
        parent: Id<Genre>,
        child: Id<Genre>,
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
        parent: Id<Genre>,
        child: Id<Genre>,
    },

    /// One genre on one subject, with why it was assigned and how confidently.
    ///
    /// **All four fields, and the previous shape had two.** It was
    /// `{ recording, genre: String }`, which threw away three things the rest
    /// of the model already had:
    ///
    /// - the **subject** could only be a recording, where `vocabulary.rs`'s
    ///   own census says the release group is the right default: genres were
    ///   found for 130 of 429 groups against 3 of 49 releases
    /// - the **genre** was a string, so it could not reach the hierarchy and
    ///   `Vocabulary::under` had nothing folded to close over
    /// - **basis and confidence** were dropped on the floor. `proposal.rs`
    ///   carried both on the wire and the conversion read
    ///   `{ recording, genre, .. } => Fact::GenreAssigned { recording, genre }`
    ///   — a Discogs guess and a typed answer became the same fact, which is
    ///   precisely what `genre-uncertain.tsv` exists to prevent
    GenreAssigned {
        subject: GenreOn,
        genre: Id<Genre>,
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

    /// A concept exists in the version vocabulary.
    QualifierCatalogued {
        concept: Id<Qualifier>,
        label: String,
    },

    /// One narrower-of edge in the version vocabulary. Row existence, as the
    /// genre hierarchy is.
    QualifierNarrowed {
        parent: Id<Qualifier>,
        child: Id<Qualifier>,
    },

    /// Which version of a song a recording is.
    ///
    /// On the **recording**, not on its link to a work — 7.3% of this library
    /// has no work and some never will, and a fact on the work link could not
    /// mark an instrumental white label at all.
    QualifierAssigned {
        recording: Id<Recording>,
        concept: Id<Qualifier>,
        basis: Source,
        confidence: Confidence,
    },

    /// Someone has decided a qualifier does not apply.
    ///
    /// **Not a delete, because there are none.** The log is append-only and
    /// merges by union, so removing an imported assignment would destroy the
    /// provenance that says where it came from, and a stale replica would
    /// deliver it again on the next sync. This shadows instead: the pair is
    /// live or not according to whichever of assign and retract carries the
    /// later stamp, which the hybrid clock already makes total.
    ///
    /// Deliberately *not* "assertions outnumber retractions" — that counting
    /// rule turns a disagreement into a war nobody can end.
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
    /// Decide a genre does not apply. Shadows rather than deletes, and folds
    /// exactly as [`Fact::QualifierRetracted`] does — same rule, one
    /// implementation, because "somebody ruled this out" is one idea and the
    /// vocabulary it is about does not change it.
    GenreRetracted { subject: GenreOn, genre: Id<Genre> },
    QualifierRetracted {
        recording: Id<Recording>,
        concept: Id<Qualifier>,
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
    retracted: &mut BTreeSet<(S, C)>,
    subject: S,
    concept: C,
    basis: Source,
    confidence: Confidence,
) where
    S: Ord + Copy,
    C: Ord + Copy,
{
    live.insert((subject, concept, basis), confidence);
    retracted.remove(&(subject, concept));
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
    retracted: &mut BTreeSet<(S, C)>,
    subject: S,
    concept: C,
) where
    S: Ord + Copy,
    C: Ord + Copy,
{
    live.retain(|(s, c, _), _| (*s, *c) != (subject, concept));
    retracted.insert((subject, concept));
}

/// Fold one claim that has no attribute to carry.
///
/// The row-existence form of [`assign`]: where an assignment splits into an
/// identity and an attribute, an edge is identity all the way down — there is
/// nothing to say about `Techno narrows Electronic` beyond that somebody said
/// it. So the key is the whole claim, and the fold is a set union.
fn assert_edge<K: Ord + Copy>(live: &mut BTreeSet<K>, retracted: &mut BTreeSet<K>, edge: K) {
    live.insert(edge);
    retracted.remove(&edge);
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
fn retract_edge<K: Ord + Copy>(live: &mut BTreeSet<K>, retracted: &mut BTreeSet<K>, edge: K) {
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
    /// Every genre assignment, **one row per asserter**.
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
    pub genres: BTreeMap<(GenreOn, Id<Genre>, Source), Confidence>,
    /// Every other name anything is known by. Row existence, so curation
    /// unions across devices the way `narrows` does.
    pub aliases: BTreeSet<(Cataloguable, String, AliasKind)>,
    /// Subjects somebody has ruled a genre out for. Keyed and folded exactly
    /// as [`Catalogue::qualifiers_retracted`] is.
    pub genres_retracted: BTreeSet<(GenreOn, Id<Genre>)>,
    /// What each concept is called.
    pub genre_names: BTreeMap<Id<Genre>, String>,
    /// Which version each recording is, **one row per asserter**, keyed and
    /// folded exactly as [`Catalogue::genres`] is.
    pub qualifiers: BTreeMap<(Id<Recording>, Id<Qualifier>, Source), Confidence>,
    /// Pairs somebody has ruled out.
    ///
    /// **Keyed without a basis, unlike the assignments it shadows.** A person
    /// saying "this is not live" is answering the question, not arguing with
    /// one asserter, so a retraction silences every basis at once — otherwise
    /// retracting would mean retracting once per source that ever said it, and
    /// the next import would put it back.
    ///
    /// Kept rather than dropped, because *decided against* and *nobody has
    /// said* are different answers and only a row can tell them apart. A later
    /// assignment clears the entry, or a retraction would be a delete wearing
    /// another name.
    pub qualifiers_retracted: BTreeSet<(Id<Recording>, Id<Qualifier>)>,
    pub qualifier_labels: BTreeMap<Id<Qualifier>, String>,
    pub qualifier_narrows: BTreeSet<(Id<Qualifier>, Id<Qualifier>)>,
    /// What each hierarchy is called.
    pub hierarchies: BTreeMap<Id<Hierarchy>, String>,
    /// Every narrower-of edge. Row existence, so curation unions.
    pub narrows: BTreeSet<(Id<Hierarchy>, Id<Genre>, Id<Genre>)>,
    /// Edges somebody has ruled out. Kept for the reason
    /// [`Catalogue::qualifiers_retracted`] is: a decision and a silence are
    /// different answers, and only a row can tell them apart.
    pub narrows_retracted: BTreeSet<(Id<Hierarchy>, Id<Genre>, Id<Genre>)>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pressed {
    pub release: Id<Release>,
    pub position: u16,
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placed {
    pub medium: Id<Medium>,
    pub recording: Id<Recording>,
    pub position: u16,
    pub title_on_release: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Grouped {
    pub title: String,
    pub artist: Id<Artist>,
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
    pub fn winner(&self) -> Id<Recording> {
        self.asserted.last().expect("a conflict has candidates").1
    }

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
        if let Fact::RecordingMerged { loser, winner } = fact {
            let _ = cat.merges.merge(*loser, *winner);
        }
    }
    let of = |r: &Id<Recording>| cat.merges.resolve(*r);
    let genre_on = |s: &GenreOn| match s {
        GenreOn::Recording(r) => GenreOn::Recording(cat.merges.resolve(*r)),
        other => *other,
    };
    let credit_on = |s: &CreditOn| match s {
        CreditOn::Recording(r) => CreditOn::Recording(cat.merges.resolve(*r)),
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
            } => {
                cat.encodings.entry(*encoding).or_insert(Encoded {
                    audio: *audio,
                    recording: None,
                    format: *format,
                    bitrate: *bitrate,
                    length_ms: *length_ms,
                });
            }
            Fact::EncodingBound { encoding, from, to } => {
                bindings
                    .entry((*encoding, from.map(|f| of(&f))))
                    .or_default()
                    .push((*stamp, of(to)));
            }
            Fact::QualifierCatalogued { concept, label } => {
                cat.qualifier_labels.insert(*concept, label.clone());
            }
            Fact::QualifierNarrowed { parent, child } => {
                cat.qualifier_narrows.insert((*parent, *child));
            }
            Fact::QualifierAssigned {
                recording,
                concept,
                basis,
                confidence,
            } => assign(
                &mut cat.qualifiers,
                &mut cat.qualifiers_retracted,
                of(recording),
                *concept,
                *basis,
                *confidence,
            ),
            Fact::QualifierRetracted { recording, concept } => retract(
                &mut cat.qualifiers,
                &mut cat.qualifiers_retracted,
                of(recording),
                *concept,
            ),
            Fact::GenreCatalogued { genre, name } => {
                cat.genre_names.insert(*genre, name.clone());
            }
            Fact::HierarchyDefined { hierarchy, name } => {
                cat.hierarchies.insert(*hierarchy, name.clone());
            }
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
            Fact::GenreAssigned {
                subject,
                genre,
                basis,
                confidence,
            } => assign(
                &mut cat.genres,
                &mut cat.genres_retracted,
                genre_on(subject),
                *genre,
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
            Fact::GenreRetracted { subject, genre } => retract(
                &mut cat.genres,
                &mut cat.genres_retracted,
                genre_on(subject),
                *genre,
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
            } => {
                cat.groups.insert(
                    *group,
                    Grouped {
                        title: title.clone(),
                        artist: *artist,
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
            Fact::ReleaseCatalogued {
                release,
                group,
                title,
                released,
            } => {
                cat.releases.insert(
                    *release,
                    Released {
                        group: *group,
                        title: title.clone(),
                        released: *released,
                    },
                );
            }
            Fact::MediumCatalogued {
                medium,
                release,
                position,
                format,
            } => {
                cat.media.insert(
                    *medium,
                    Pressed {
                        release: *release,
                        position: *position,
                        format: format.clone(),
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
            Fact::RecordingMerged { .. } => {}
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

impl Catalogue {
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
    pub fn unplaced_genres(&self, hierarchy: Id<Hierarchy>) -> BTreeSet<Id<Genre>> {
        let placed: BTreeSet<Id<Genre>> = self
            .narrows
            .iter()
            .filter(|(h, _, _)| *h == hierarchy)
            .flat_map(|(_, parent, child)| [*parent, *child])
            .collect();
        self.genre_names
            .keys()
            .filter(|g| !placed.contains(g))
            .copied()
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
        ("label", None),
        ("genre", Some("GenreCatalogued")),
        ("qualifier", Some("QualifierCatalogued")),
        ("hierarchy", Some("HierarchyDefined")),
        ("role", Some("RoleDefined")),
        ("collection", None),
        ("credit", Some("CreditAsserted")),
        ("mix-entry", None),
        ("collection-entry", None),
        ("genre-assignment", Some("GenreAssigned")),
    ];

    /// **The allowlist only shrinks.**
    ///
    /// A kind with no fact cannot be created by folding the log, so whatever
    /// produces it is producing something the library cannot record, correct,
    /// attribute or sync. Filling one in is progress; adding one is a
    /// regression, and this is where it is caught.
    #[test]
    fn every_entity_is_reachable_from_a_fact_or_named_as_a_gap() {
        const UNREACHABLE: usize = 4;

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
