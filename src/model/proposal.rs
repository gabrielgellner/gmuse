//! What a tool says to the library — `library-api.md` §5.
//!
//! A tool proposes in bulk, a person reviews the file, and gmuse applies it.
//! One JSON object per line, because facts are heterogeneous: a TSV would need
//! a column union across every kind of thing anyone might ever want to assert,
//! and this project has already been bitten by a TSV whose quoting fought its
//! own content.
//!
//! ```jsonl
//! {"fact":"measured","path":"…/03 Callisto.mp3","cutoff_hz":19800,"analyser":1}
//! {"fact":"genre_assigned","path":"…/03 Callisto.mp3","genre":"Techno","basis":"discogs","confidence":"matched"}
//! ```
//!
//! ## A tool names an identity, not a file
//!
//! A proposal says `"encoding":"…uuid…"`, never `"path":"/m/a.mp3"`. A path is
//! a property of one device's copy: it is stale the moment a file moves, it
//! means nothing on another machine, and keying anything by one is the failure
//! this whole model exists to end.
//!
//! The objection that a tool cannot *compute* an id — `audio_id` is XXH3 over
//! Symphonia's packet boundaries and is not reproducible in Python — is real
//! and answered by not asking it to. A tool does not compute an identity, it
//! **receives** one:
//!
//! ```text
//! gmuse library query … --format jsonl   rows carrying the id and a locator
//!        ↓
//! the tool opens the locator, measures, and proposes against the id
//!        ↓
//! gmuse library apply proposals.jsonl
//! ```
//!
//! So the path appears in query *output*, where it is a thing to open, and
//! never in proposal *input*, where it would be an identifier. An id that no
//! longer names anything is reported, never guessed at.

use std::collections::BTreeSet;

use crate::model::basis::Basis;
use crate::model::clock::HybridClock;
use crate::model::entity::{
    Artist, Collection, Concept, Device, Encoding, Facet, Medium, Recording, Release, ReleaseGroup,
    Role, Track,
};
use crate::model::facet::{self, Described};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::log::{Fact, Record};
use crate::model::ord::OrdKey;
use crate::model::reference::{CreditOn, RoleClass, Source};

/// One assertion, as a tool writes it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum Proposal {
    /// What an analyser found about **an encoding**. `quality.py`'s output.
    ///
    /// The subject is the encoding rather than the recording, because a cutoff
    /// is a fact about particular bytes: two encodings of one performance
    /// measure differently, and that difference is the whole point.
    Measured {
        encoding: Id<Encoding>,
        cutoff_hz: u32,
        analyser: u16,
    },
    /// A genre, and where it came from. The genre pipeline's output — and the
    /// reason `basis` and `confidence` are on the wire rather than implied:
    /// a Discogs guess and a typed answer are the same shape and must not be
    /// the same fact.
    GenreAssigned {
        recording: Id<Recording>,
        genre: String,
        basis: Source,
        confidence: Confidence,
    },
    /// **This file holds that performance** — the proposal 712 confident
    /// matches have been waiting for.
    ///
    /// The subject is the **encoding**, not the recording, and that is the
    /// whole shape of it. A recording that nothing has named was minted from
    /// its bytes, so its id *is* the claim being corrected; naming the
    /// recording would mean naming the thing this replaces. The file is what
    /// stays put.
    ///
    /// Applying one is therefore a **relink**, not an append: the encoding's
    /// binding moves from the id the bytes minted to the id the authority
    /// names. The old recording is left rather than swept — §1 is explicit
    /// that a row nothing holds is ordinary, and deleting it would take its
    /// ratings and plays with it.
    ///
    /// Nothing about the recording's *description* crosses. An identification
    /// says which performance this is, and a title from MusicBrainz is a
    /// separate claim somebody else can propose.
    Identified {
        encoding: Id<Encoding>,
        basis: Basis,
    },
    /// Which version of a song a recording is — issue #101.
    ///
    /// The route that replaces a stopgap. `identify.py` currently *downgrades*
    /// a match whose title carries a version qualifier ours does not mention,
    /// because there was nowhere to record what it had noticed: an
    /// instrumental and its vocal are the same length by construction and
    /// score 100 on the name, so both checks the matcher has say "same". Three
    /// of 607 confident matches were exactly this.
    ///
    /// Downgrading throws the observation away. Proposing keeps it, and a
    /// person reviewing the file decides.
    QualifierAssigned {
        recording: Id<Recording>,
        /// The concept's label, not its id. A tool should not have to mint —
        /// `Id::of_label` is the model's job, and a label is what a person
        /// reviewing the file can read.
        qualifier: String,
        basis: Source,
        confidence: Confidence,
    },
    /// **This recording belongs to that collection** — #94.
    ///
    /// The route a path-based catalog takes to become a curated set. Measured
    /// on this library, no genre rule reproduces `~/MusicLibrary/Dance`: every
    /// one of its 790 files carries an `Electronic` term, and so do 876 files
    /// in `General` — Laurie Anderson, Beck, Enya, DEVO. Those are correctly
    /// tagged and are simply not what the folder means.
    ///
    /// What the folder records is not a genre but a judgement: *this is dance
    /// music and I listen to it as dance music*. Enya is `Electronic` and is
    /// not in it. That belongs in a collection, whose membership is facts
    /// (#92) — and once it is, the path stops mattering, which is what #90
    /// asks for and what #94's second requirement is waiting on.
    ///
    /// The collection is named rather than identified, for the same reason a
    /// genre is: a tool has no way to mint an id and should not, and a name is
    /// what a person reviewing the file can read.
    CollectedInto {
        recording: Id<Recording>,
        collection: String,
        /// Whether the collection's order means anything. A catalog seeded
        /// from a folder is a set somebody curated, not a running order.
        #[serde(default)]
        ordered: bool,
    },
    /// **One genre is narrower than another** — the hierarchy #79 can group by
    /// and #93 can close over, but that nothing produced.
    ///
    /// The curation the library was already doing in a tag string. Its
    /// `Broad; Style` convention exists because a view could not name a level
    /// of a vocabulary; putting the edges in the model is what lets the
    /// compound string stop being load-bearing.
    ///
    /// **Both terms are labels, not ids**, for the same reason a genre and a
    /// collection are: a tool has no way to mint one and should not, and a
    /// label is what a person reviewing the file can read.
    ///
    /// The edge goes into the facet's canonical array — `Id::<Facet>::array`.
    /// A vocabulary may carry several hierarchies, but nothing yet has a
    /// second one to propose into, and a name nobody uses is a field to get
    /// wrong.
    Narrowed {
        /// The broader term. `Electronic`, for a child of `Techno`.
        broader: String,
        narrower: String,
    },
    /// **A file that was made out of another file** — #132.
    ///
    /// The route the thing that *makes* a derived copy uses to say so. A
    /// transcode is a fact somebody performed, and nothing downstream can
    /// infer it: the bytes do not say where they came from, and a format does
    /// not either — a purchased 320 kbps MP3 looks exactly like one you made.
    ///
    /// Both are encodings, by id, because a tool that made a file knows the
    /// file it made it from. Neither is a name to be resolved.
    Derived {
        from: Id<Encoding>,
        to: Id<Encoding>,
    },
    /// **One recording was made out of another** — #162, and the first thing
    /// to mint a `RecordingDerivation`.
    ///
    /// [`Proposal::Derived`] one level up, and a different kind of act. That
    /// one is a transcode: the same performance, encoded again. This is a
    /// musical decision — a remix, an edit, a dub — and the result is a
    /// *different recording* that a different person made.
    ///
    /// **A cover is deliberately not expressible here.** It realises the
    /// existing work as any performance does, and putting it in the derivation
    /// graph would make "which versions of this recording" answer with things
    /// that were never made out of it. A cover is two recordings of one
    /// `Work`, and that is the level it belongs to.
    ///
    /// **The far end is named, not held** — an archivist, not a completionist.
    ///
    /// A derivation names both ends, and the original is routinely something
    /// this library does not have: you own the remix. `library-api.md` §1
    /// settles what to do about that — **a reference to something the library
    /// does not hold is a fact about the world, not a gap to fill** — and
    /// `Ref<T>` said it all along: *a reference is valid when its target is
    /// absent; resolution is an optimisation, not a precondition.* The first
    /// version of this arm refused on exactly that, and was wrong to.
    ///
    /// **So `from` is a `Basis` and not an id.** The authority's name for the
    /// original, which the model mints — the same shape
    /// [`Proposal::Identified`] takes, and for the same reason: a tool does
    /// not compute an identity, it receives one. `Basis::Audio` covers a
    /// derivation between two recordings nobody has named.
    ///
    /// It costs nothing in the browse. `draw` builds rows from
    /// `RecordingCatalogued`, so a recording named only by
    /// `RecordingIdentified` has no title and **is never drawn** — the
    /// reference is traversable, nameable in an info panel, and invisible in a
    /// list, by construction rather than by a rule. And because
    /// `Id::of_basis` is convergent, the id this mints is the id an
    /// acquisition would mint: the edge resolves itself if the original ever
    /// arrives.
    RecordingDerived {
        from: Basis,
        to: Id<Recording>,
        kind: crate::model::spine::RecordingDerivationKind,
    },
    /// **Withdraw a hierarchy edge** — the other half, and not optional.
    ///
    /// The log is append-only, so a curation pass that revises itself leaves
    /// its first draft standing and the two union. That is not academic: this
    /// library's first pass put `Doom Metal` under `Rock`, the second put it
    /// under `Metal`, and after both it was directly under *both* — so
    /// `genre@1` reported `Metal` as a broad term because the shortcut edge
    /// made it one hop from the top.
    ///
    /// Shadows rather than deletes, like every retraction here: a stale
    /// replica re-delivering the edge would otherwise resurrect it.
    /// **This credit names several artists** — #152, and the repair for a
    /// credit that became one.
    ///
    /// Without a MusicBrainz artist id an artist is minted from the credit
    /// *string*, so `Armand Van Helden,Dizzee Rascal` is an artist and Stephan
    /// Bodzin is four people. The model was always ready for the fix —
    /// `CreditAsserted` carries a `position` and a `join_phrase` precisely so
    /// a multi-artist credit is several rows — and what was missing is a way
    /// for a pass to say so.
    ///
    /// **Named, not identified**, like a genre and a collection: a tool has no
    /// business minting an artist id, and a name is what a person reviewing
    /// the file can read. The ids come out of `Id::of_artist`, so two devices
    /// applying the same row land on the same artists.
    ///
    /// **The joined artist is left alone.** Its credit is superseded by these,
    /// but the row itself stays, because §1 is explicit that a row nothing
    /// holds is ordinary and deleting one takes its ratings and plays with it.
    /// A split is repaired by a merge (#88); a merge is repaired by nothing,
    /// which is why this proposes rather than the pass deciding.
    Credited {
        recording: Id<Recording>,
        /// The artists the credit names, **in the order it names them** — the
        /// position is what `CreditAsserted` keys a row by, so a reordering is
        /// a different set of rows rather than an edit.
        artists: Vec<String>,
        /// Where the answer came from, on the wire for the reason it is on a
        /// genre: MusicBrainz naming these two and a person typing them are
        /// the same shape and must not become the same fact.
        basis: Source,
    },
    /// **One person did one thing on this recording** — #162.
    ///
    /// [`Proposal::Credited`] is the *split*: "this credit names several
    /// artists", which is why it refuses a single name and why it asserts
    /// `performer`. This is the other claim — **one artist, one role** — and
    /// the two are not the same statement wearing different clothes.
    ///
    /// The case it was built for: `identify` matches a remix and the
    /// candidate's own title names the remixer our title named. That agreement
    /// is evidence no tag provides alone, and 57 of them were computed and
    /// discarded because nothing could say them.
    ///
    /// **The role must be one the codelist knows.** A codelist grows by fact
    /// and `RoleDefined` will take any name — but it takes a *class* with it,
    /// and a class cannot be inferred from a string. A `remixer` filed as
    /// `Performer` is worse than no credit at all: `line_of` reads performers,
    /// so it would put the record in the remixer's folder, which is the bug
    /// that made the role filter exist.
    ///
    /// **Named, not identified**, as every artist on this wire is: minting is
    /// the model's job and a name is what a person reviewing the file reads.
    Contributed {
        recording: Id<Recording>,
        artist: String,
        /// `remixer`, `producer`, `featured`, `composer`. One of
        /// [`crate::model::reference::ROLES`].
        role: String,
        basis: Source,
    },
    /// **This artist is not on this record** — the withdrawal #145 left
    /// needing, and the only thing that can undo a joined credit.
    ///
    /// A joined credit mints one artist nobody is called — `Stephan Bodzin &
    /// Marc Romboy` as one act — and asserts it on the album. #145 stopped new
    /// ones being minted; nothing could withdraw the 356 already here.
    ///
    /// **Names the artist by id, not by name**, and that is the difference
    /// from [`Proposal::Contributed`]. A contribution mints the artist it
    /// credits, so a name is the right input. A withdrawal is about a row that
    /// already exists and is usually *wrongly named* — minting from the string
    /// would derive a fresh id and retract a credit nobody ever asserted.
    /// `shelf artists` and `shelf names` are what print the id.
    ///
    /// **The subject is a recording or an album.** A joined credit lands on
    /// both — `file_under` asks the album first, which is how one bad credit
    /// renames a folder.
    CreditWithdrawn {
        subject: CreditOn,
        artist: Id<Artist>,
        /// One of [`crate::model::reference::ROLES`], as the credit was
        /// asserted.
        role: String,
        /// **Whose decision this is.** A retraction shadows every basis at
        /// once, but the rung still matters: a person's correction must not be
        /// quietly undone by the next scan.
        basis: Source,
    },
    /// **These two rows are one performance** — #88, and the repair a split
    /// leaves needing.
    ///
    /// A recording splits whenever two copies of one piece of music are named
    /// differently: a FLAC bought against an MP3 already held, the two tagged
    /// from different MusicBrainz releases, is the ordinary case. Both ids are
    /// *correct* — MusicBrainz really does hold two recordings — and the
    /// library holds one piece of music twice.
    ///
    /// **A merge carries everything, which is why it is the right instrument
    /// and a relink is not.** `RecordingMerged` folds in a pre-pass and every
    /// fact after it resolves its recording through the result, so ratings,
    /// plays, collection memberships, concepts and credits all follow the
    /// winner — and so does the loser's encodings' binding. `Identified` moves
    /// a binding and merges *only* where the old id was minted from bytes,
    /// because an authority having named it means the two are different
    /// performances. Where they are not, this is the fact that says so.
    ///
    /// **The direction is on the wire because it is not inferable.** A merge is
    /// not undone by another merge, and the wrong winner hides the id
    /// everything else already points at.
    Merged {
        loser: Id<Recording>,
        winner: Id<Recording>,
    },
    NarrowingRetracted {
        broader: String,
        narrower: String,
    },
    /// **These two album entries are one album** — #156.
    ///
    /// [`Proposal::Merged`] one level up, and needed for the same reason: a
    /// group's id is a function of what the tags said, so learning an album's
    /// MusicBrainz id changes its identity and `import` mints a second album
    /// beside the first. 33 albums on this library are held twice that way.
    ///
    /// **Groups only, and that is the decision.** It says nothing about the
    /// pressings underneath: *Barton Hollow* is a standard edition of 14
    /// tracks and a deluxe with 4 more, and the answer wanted is *one album
    /// holding two releases*. A merge that folded downward would erase the
    /// level #134 exists to hold.
    ///
    /// **The direction is on the wire because it is not inferable**, exactly
    /// as it is for a recording — and here there is a second reason to make a
    /// person say it: not every duplicate title is one album. `AC/DC — High
    /// Voltage` is the 1975 Australian record and the 1976 international one,
    /// sharing a name and a folder, and they are correctly two.
    GroupsMerged {
        loser: Id<ReleaseGroup>,
        winner: Id<ReleaseGroup>,
    },
    /// **These two ids are one artist** — the same repair one level sideways,
    /// and the largest of the three splits this library actually has.
    ///
    /// `of_artist` mints from a MusicBrainz id where the tags carry one and
    /// from the name otherwise, so one artist arriving both ways mints two.
    /// Of 1015 named artists here, **82 names are held by more than one id**.
    ///
    /// **The direction is on the wire**, for the reason it is at every other
    /// level: it is not inferable. Here the caller has a rule — the
    /// MusicBrainz-derived id is the higher rung and wins — but a rule the
    /// tool applies is still a claim the file has to state, or a person
    /// reading the proposal cannot see which way it went.
    ///
    /// **Two MusicBrainz ids sharing a name are not a split.** `Christian
    /// Löffler` is held under two here, and MusicBrainz saying two artists is
    /// evidence for two. Nothing in this refuses that pair — the refusal is
    /// the emitter's job, because only it knows how each id was derived — but
    /// it is the pair to think about before proposing a name-matched merge.
    ArtistsMerged {
        loser: Id<Artist>,
        winner: Id<Artist>,
    },
    /// **This artist is called that** — #154.
    ///
    /// The one thing no proposal could say. `Instance::rename_artist` has
    /// written the right fact all along with nothing to drive it, so the model
    /// half was built and the wire half was not.
    ///
    /// Two cases want it, and they are the two ends of the same guard.
    ///
    /// **An artist with a right id and no label.** #145 withholds the name
    /// when a credit is joined, because the id is the *primary* artist's and
    /// the string names the whole credit. Correct, and it leaves Tony Furtado
    /// identified by his MusicBrainz id, named by nothing, and unreachable in
    /// a browse — the library knows exactly who he is and cannot say it.
    ///
    /// **And an artist the guard failed to protect.** Where it did not fire —
    /// in Vorbis, until #158 — a joined credit renamed the artist it merely
    /// starts with, and `ArtistNamed` is last-writer-wins, so Dolly Parton is
    /// called `Dolly Parton, Linda Ronstadt & Emmylou Harris` and six of her
    /// albums file under it. **A re-import does not repair that**: the right
    /// name is already in the log, so it is dropped as a repeat and the wrong
    /// one goes on being the last word. Only a new assertion can outrank it.
    ///
    /// **A name, not a rule.** Nothing here parses `Tony Furtado feat. Alison
    /// Krauss` back into two names — #152 is that question, and its finding is
    /// that parsing credits is unsafe. The id is already right; only the label
    /// is owed.
    Named {
        artist: Id<Artist>,
        name: String,
    },
    /// **This recording is called that** — the twin of [`Proposal::Named`] one
    /// level down, and the thing an identification could not say.
    ///
    /// An identification moves an encoding's binding and merges what the old
    /// id carried, which is right — except when the *title* was the error.
    /// `James Blake — The Bells Sketch` and `Give a Man a Rod` are one folder
    /// with their tags transposed; two fingerprints name the other's title at
    /// 0.98 each, and identifying them fixed both bindings and left both
    /// labels wrong.
    ///
    /// **Separate from the identification on purpose.** A relink is a claim
    /// about which performance the bytes hold; a title is a claim about what
    /// it is called, and most relinks should not touch it — the tags were
    /// right about the name and only vague about the id. Folding the two
    /// together would make every identification a rename, which is how a
    /// careful tag gets overwritten by whichever spelling MusicBrainz happens
    /// to prefer.
    ///
    /// So the tool sends it only where it has reason to, and a person reading
    /// the file sees a rename as a rename.
    ///
    /// **`identify fingerprint` sends one where the names differ**, because
    /// there the record was found by *audio* and a differing title is
    /// therefore something learned. `propose` never sends one: it found the
    /// record by title, so the two agree by construction.
    Retitled {
        recording: Id<Recording>,
        title: String,
    },
    /// **The whole tracklist of one medium** — #155.
    ///
    /// The proposal that lets the library hold *what a record is* rather than
    /// only what it can play. Identification already takes a release's id and
    /// then throws away the tracklist that came with it: having matched
    /// `Adele — 21` against an 18-track release, the library can say it holds
    /// 15 files and cannot name the three it is missing.
    ///
    /// **A slot with a recording and no encoding is how this model already
    /// says "this track exists and I do not have it."** Nothing had ever
    /// written one, because `import` writes a slot and a copy together and so
    /// the distinction was never exercised.
    ///
    /// ## Named by MusicBrainz ids, and the model mints
    ///
    /// Like [`Proposal::Identified`], which sends a `Basis` rather than an
    /// `Id<Recording>`: a tool has no business minting, and every id here is a
    /// pure function of what it sends. The release mints the medium, the
    /// recording mints the slot, and two devices given the same tracklist land
    /// on the same entities without a conversation.
    ///
    /// ## The medium must already exist, and that is the safety property
    ///
    /// The subject is the medium, so a tracklist for a record the library has
    /// never seen is **stale rather than applied**. That matters because the
    /// release id has to agree with the one `import` minted, and it only does
    /// where the files carried the album's MusicBrainz id: a file tagged
    /// without one mints its release from the *group*
    /// ([`Id::<Release>::of_release`]), and a tracklist arriving against the
    /// real MBID would then catalogue a whole phantom disc beside the real
    /// one. Refusing is loud; a second copy of the album is not.
    ///
    /// ## What it costs
    ///
    /// "Contains" changes meaning. Afterwards the library holds catalogue
    /// entries as well as files, so a view that queues a row has to know which
    /// it has — and a wrong release now **invents slots for tracks that are
    /// not on your edition** rather than merely mistagging files you own.
    /// That is the price of a `close` match accepted by hand.
    Tracklisted {
        /// The MusicBrainz release. Not a group: a tracklist belongs to one
        /// pressing, which is the whole reason #134 put the level in.
        release: uuid::Uuid,
        /// Which disc of it, counting from 1.
        position: u16,
        /// `CD`, `12" Vinyl`, `Digital Media` — carried because this fact
        /// supersedes the medium wholesale and dropping a field would clear
        /// it. `None` where the authority did not say.
        #[serde(default)]
        format: Option<String>,
        /// Every slot on it, **in full**. The length is the denominator #134
        /// asked for, so a partial list would claim the record is shorter than
        /// it is — which is a worse answer than "nobody said".
        tracks: Vec<Slotted>,
    },
    /// A rating. Included because it is the simplest possible proposal and
    /// therefore the one worth testing the format against.
    Rated {
        recording: Id<Recording>,
        stars: Option<u8>,
    },
}

/// One slot in a [`Proposal::Tracklisted`] — a track on the record, held or
/// not.
///
/// Deliberately not [`crate::model::rows::Listed`], which is a *drawn row*.
/// This is what an authority says sits at a position, before the library has
/// any opinion about whether it can play it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Slotted {
    /// Where on the disc, as the authority numbers it.
    pub position: u16,
    /// The **recording** — the performance filling the slot, not the track
    /// entry that names it. MusicBrainz has both and they are different ids;
    /// the recording is what a copy of this song anywhere else would also be,
    /// and so it is what makes the slot converge with a file we later buy.
    pub recording: uuid::Uuid,
    pub title: String,
    /// **What the sleeve prints here, where that is not the recording's own
    /// name** — and `None` on nearly every row, which is the point.
    ///
    /// MusicBrainz carries both titles and the tool compares them, which is
    /// what this field is waiting on: it used to be `None` unconditionally
    /// because "a tracklist read straight off the release cannot tell the
    /// difference". It can; it just was not looking.
    ///
    /// **Sent only for a difference that is not punctuation.** Measured on
    /// *Luna*, 3 of 34 slots differ and two of those are a non-breaking hyphen
    /// and a curly apostrophe — `Phobos (Pan‐Pot remix)` against
    /// `Phobos (Pan-Pot remix)`. Claiming a rename for those would put one on
    /// rows where MusicBrainz merely normalised a dash. The third is real:
    /// the recording is `Phobos` and *Luna* prints `Phobos (Synthapella)`.
    #[serde(default)]
    pub title_on_release: Option<String>,
    /// Who it is credited to, **in the order the credit names them**.
    ///
    /// A list rather than the joined line, and that is not a nicety. A joined
    /// string mints one artist called `Armand Van Helden,Dizzee Rascal`, which
    /// is the bug #152 exists to repair — and asserted here on the MusicBrainz
    /// rung it would outrank the tag it came from and make the repair harder,
    /// not easier.
    ///
    /// Sent as a list, a tracklist emits exactly the facts
    /// [`Proposal::Credited`] emits, so the two **converge** on one set of
    /// rows instead of rendering a credit line twice over.
    pub artists: Vec<String>,
}

/// What `query --format jsonl` hands a tool.
///
/// The other half of the contract, and the reason a proposal never carries a
/// path: everything a tool needs to do its work and then say what it found
/// arrives here. The **id** is what it will propose against; the **locator**
/// is what it will open.
///
/// They are deliberately different fields with different lifetimes. The
/// locator is good until the file moves; the id is good forever.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Queried {
    /// **Absent for a row the library has not named** — #160.
    ///
    /// The gate holds a file's bytes without minting a performance, so there
    /// is genuinely no recording to name. A pass working that list proposes
    /// against the `encoding`, which is what an identification names anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<Id<Recording>>,
    /// The encoding a tool would measure — the canonical one, unless the query
    /// asked for something else.
    pub encoding: Option<Id<Encoding>>,
    /// Where this machine can open it, if it can. Absent for a recording
    /// nothing here holds, which is an ordinary row.
    pub locator: Option<String>,
    pub title: String,
    pub artist: String,
    /// **How long it runs, measured** — and the reason it is here rather than
    /// left to the tool that wants it.
    ///
    /// Every pass that uses a duration was opening the file and asking a
    /// tagging library, which computes one from the Xing header where there is
    /// one and from `size / first frame's bitrate` where there is not. That is
    /// wrong by up to 26 minutes on this library, and it is wrong in the
    /// *same* way in every tool, because they all ask the same kind of
    /// question of the same kind of parser.
    ///
    /// `audio_id` counts the frames at import, which is exact. Carrying the
    /// answer here is what stops each pass re-deriving it badly — the same
    /// reason a work list carries an id rather than letting a tool mint one.
    ///
    /// `None` for a row this device holds no copy of, which is the only case
    /// where nothing has been measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length_ms: Option<u32>,
}

/// What `parse` needs to know about the library it is proposing against.
///
/// It was a single `holds` closure, and that was enough while every proposal
/// was a function of its own line. An identification is not: it moves an
/// encoding's binding, and where it moves *from* is a fact about the library
/// now. A tool cannot supply that — it would be naming the thing its proposal
/// replaces — so the caller answers instead.
pub trait Holdings {
    /// Whether the library has this at all. A proposal against a library that
    /// has since changed is stale rather than wrong.
    fn holds(&self, subject: Subject) -> bool;
    /// Which recording an encoding currently holds.
    fn bound(&self, encoding: Id<Encoding>) -> Option<Id<Recording>>;
    /// What a recording's id was derived from.
    ///
    /// The rung is what separates a repair from a claim: a recording resting
    /// on its bytes is one nothing had named, so identifying it merges. One an
    /// authority already named is a different performance, and a file pointed
    /// at it is a relink with no merge in it.
    fn identified(&self, recording: Id<Recording>) -> Option<Basis>;
    /// The slot this copy fills, and what that slot claims, where it fills one.
    ///
    /// **Two facts name a recording**, and a relink has to move both: a file
    /// says which performance it holds (`EncodingBound`) and the slot it fills
    /// says which performance sits there (`TrackPlaced`). Changing only the
    /// first leaves the appearance naming the song the file used to be, so a
    /// rating given to the row went to the old song.
    fn slot(&self, encoding: Id<Encoding>) -> Option<Slot>;
    /// **What these bytes claimed, for an encoding nothing has named** — #160.
    ///
    /// `None` once a recording is bound, because then the library has a
    /// catalogue entry and a claim is not needed to build one. Present exactly
    /// for the rows the gate held back, which is what lets an identification
    /// finish the job instead of leaving a recording with no title.
    fn claim(&self, encoding: Id<Encoding>) -> Option<(String, String)>;
}

/// Where a copy sits, as the library currently records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub track: Id<Track>,
    pub medium: Id<Medium>,
    pub position: u16,
}

/// What a proposal is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Encoding(Id<Encoding>),
    Recording(Id<Recording>),
    /// **A person or an act** — #154.
    Artist(Id<Artist>),
    /// **An album** — #156. Its own subject rather than a recording's,
    /// because a merge at this level says nothing about the songs on it.
    ReleaseGroup(Id<ReleaseGroup>),
    /// **A disc of a pressing** — #155, and the only subject that is not a
    /// thing you can play.
    ///
    /// A tracklist is about the record, not about any song on it. Gating it on
    /// a recording would be asking the wrong question twice over: the slots it
    /// writes are for recordings the library has never heard of, and the one
    /// thing that *must* already exist is the disc they go on.
    Medium(Id<Medium>),
    /// **The vocabulary itself**, which the library always has (#79).
    ///
    /// A hierarchy edge names two concepts and no holding. Gating it on "does
    /// the library hold this" would be asking the wrong question: curation of
    /// the codelist is not about what is on the disk, and a term nothing is
    /// tagged with yet is exactly the one a hierarchy is being built ahead of.
    Vocabulary,
}

impl Proposal {
    pub fn subject(&self) -> Subject {
        match self {
            Proposal::Measured { encoding, .. } | Proposal::Identified { encoding, .. } => {
                Subject::Encoding(*encoding)
            }
            Proposal::GenreAssigned { recording, .. }
            | Proposal::QualifierAssigned { recording, .. }
            | Proposal::Rated { recording, .. }
            | Proposal::CollectedInto { recording, .. }
            | Proposal::Credited { recording, .. }
            | Proposal::Contributed { recording, .. } => Subject::Recording(*recording),
            // **Whatever the credit is on**, because that is the row that has
            // to exist for a withdrawal to mean anything.
            Proposal::CreditWithdrawn { subject, .. } => match subject {
                CreditOn::Recording(r) => Subject::Recording(*r),
                CreditOn::ReleaseGroup(g) => Subject::ReleaseGroup(*g),
                // **A work and a pressing have no subject to check against.**
                // Nothing in this library credits either — `ingest` credits
                // the recording and the group, and #145's joined credits are
                // on those two. Treated as always-held rather than refused,
                // because refusing a shape nothing produces would be a rule
                // written for a case that has never arrived.
                CreditOn::Work(_) | CreditOn::Release(_) => Subject::Vocabulary,
            },
            // **The winner**, because that is the row that has to exist for the
            // merge to mean anything. A loser nothing holds is ordinary.
            Proposal::Merged { winner, .. } => Subject::Recording(*winner),
            Proposal::Narrowed { .. } | Proposal::NarrowingRetracted { .. } => Subject::Vocabulary,
            // **The thing it is about is the derived file.** The source is
            // checked below, where a missing one is a reason to refuse rather
            // than a subject the library has not heard of.
            Proposal::Derived { to, .. } => Subject::Encoding(*to),
            // **Minted here, not sent.** The tool names a MusicBrainz release
            // and a disc number; the id is a pure function of those two, and
            // asking a Python script to reproduce `of_medium`'s seeding would
            // be a second door into the same identity.
            // **The winner**, for the reason a recording merge names one:
            // that is the row that has to exist for the merge to mean
            // anything. A loser nothing holds is ordinary.
            Proposal::GroupsMerged { winner, .. } => Subject::ReleaseGroup(*winner),
            // **The winner**, for the reason the other two merges name one:
            // that is the row that has to exist. A loser nothing holds is
            // ordinary — it is what half a split looks like from the other
            // side.
            Proposal::ArtistsMerged { winner, .. } => Subject::Artist(*winner),
            Proposal::Named { artist, .. } => Subject::Artist(*artist),
            // **The recording has to exist.** A title is a description of
            // something held, and a retitle naming a row nothing points at is
            // stale rather than wrong.
            Proposal::Retitled { recording, .. } => Subject::Recording(*recording),
            // **The derived recording**, for the reason `Derived` names its
            // `to`: that is the row this is about. The original is checked in
            // the arm below, where a missing one is a reason to refuse rather
            // than a subject nobody has heard of.
            Proposal::RecordingDerived { to, .. } => Subject::Recording(*to),
            Proposal::Tracklisted {
                release, position, ..
            } => Subject::Medium(Id::<Medium>::of_medium(
                Id::<Release>::of_musicbrainz(*release),
                *position,
            )),
        }
    }
}

/// What applying a proposal file did.
#[derive(Debug, Default)]
pub struct Report {
    pub applied: usize,
    /// Lines that were not JSON, or were JSON of the wrong shape. Named with
    /// their line number, because a proposal file is something a person edits
    /// and an editor needs somewhere to jump to.
    pub malformed: Vec<(usize, String)>,
    /// Subjects the library does not hold. A proposal written against a
    /// library that has since changed is stale, not wrong, and saying which
    /// ones is what lets a tool re-query rather than re-measure.
    pub unknown: Vec<Subject>,
    /// Lines the vocabulary refused, with why.
    ///
    /// Distinct from `malformed`, because the line parsed and the subject is
    /// held — what is wrong is the claim. `qualifier = mix-variant` asserts a
    /// grouping concept, which says nothing about a recording and would fold
    /// to an assignment nobody can see; refusing is better than accepting,
    /// because an assignment nobody can see reads as one nobody made.
    pub refused: Vec<(usize, String)>,
}

impl Report {
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — an invariant a test asserts, not an answer anything needs at runtime"
    )]
    pub fn balances(&self, lines: usize) -> bool {
        lines == self.applied + self.malformed.len() + self.unknown.len() + self.refused.len()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} applied, {} unknown, {} refused, {} malformed",
            self.applied,
            self.unknown.len(),
            self.refused.len(),
            self.malformed.len()
        )
    }
}

/// Turn a proposal file into records.
///
/// Blank lines and `#` comments are skipped, because a reviewed file is one a
/// person has been editing — and a note beside a row they were unsure about is
/// the point of reviewing it.
///
/// **Tolerated on the way in, never written on the way out.** A comment is not
/// part of JSON Lines, so a file carrying one is refused by `fx`, by `jq`
/// without `-R`, and by every other reader that takes the format at its word.
/// The tools used to write a provenance header and it cost exactly that: the
/// files they produced could not be opened by the things one opens JSONL with.
/// Liberal in what is accepted, strict in what is emitted.
///
/// **Nothing is partially applied.** A malformed line does not stop the others
/// — that would make a typo cost a whole pass — but it is reported, and a
/// caller that wants all-or-nothing checks the report before appending.
pub fn parse(
    text: &str,
    device: Id<Device>,
    now: i64,
    library: &impl Holdings,
) -> (BTreeSet<Record>, Report) {
    let mut clock = HybridClock::new(device);
    // Keyed by rendering, because `Fact` is neither `Hash` nor `Ord` and a
    // declaration's identity is exactly what it says.
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = BTreeSet::new();
    let mut report = Report::default();

    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let proposal: Proposal = match serde_json::from_str(line) {
            Ok(p) => p,
            Err(e) => {
                report.malformed.push((n + 1, e.to_string()));
                continue;
            }
        };
        if !library.holds(proposal.subject()) {
            report.unknown.push(proposal.subject());
            continue;
        }
        let facts = match into_facts(proposal, library) {
            Ok(facts) => facts,
            Err(why) => {
                report.refused.push((n + 1, why));
                continue;
            }
        };
        for fact in facts {
            // **A concept is declared once per file, not once per line.**
            //
            // Every assignment brings its concept with it, which is right —
            // an id no fact resolves is an id nobody can render. But a file of
            // 761 proposals into one collection carried 761 identical
            // `CollectionNamed` records, and a file of genre assignments does
            // the same with `GenreCatalogued`. They fold to one row and cost a
            // durable line each; the Dance seeding put 761 of them into a log
            // of 7,974.
            //
            // **Only the declarations**, and the narrowing is a guard rather
            // than a fix. No proposal produces a fact that has to survive
            // twice — a play is recorded by the player, not proposed — so
            // deduplicating everything would behave identically today, and a
            // mutation making it do so passes every test here.
            //
            // It is still the right shape. `RecordingPlayed` carries no
            // timestamp, so two plays of one track are the same fact told
            // apart only by their stamps; the day something proposes one, a
            // blanket rule would count one play where there were two, and
            // silently. `declares_a_concept` is what makes adding that a
            // deliberate act rather than an accident.
            if declares_a_concept(&fact) && !declared.insert(format!("{fact:?}")) {
                continue;
            }
            out.insert(Record {
                stamp: clock.stamp(now),
                fact,
            });
        }
        report.applied += 1;
    }
    (out, report)
}

/// Whether a fact only *declares* something, so saying it twice says nothing.
///
/// These name a concept so that an id resolves to something a person reads.
/// They are pure upserts: two identical ones fold to one row, and the second
/// buys a line in a durable log and nothing else.
///
/// **Kept to a list rather than inferred.** "Idempotent" is not a property the
/// type system carries here — `RecordingPlayed` is *counted*, and it holds no
/// timestamp, so two plays of one track are the same fact told apart only by
/// their stamps. Anything added to this list has to be checked, not guessed.
fn declares_a_concept(fact: &Fact) -> bool {
    matches!(
        fact,
        Fact::ConceptCatalogued { .. } | Fact::CollectionNamed { .. }
    )
}

/// One proposal becomes the facts it implies, or says why it cannot.
///
/// **More than one, because a concept has to exist before it can be
/// assigned.** This used to return a single fact, and an applied genre
/// proposal for a term the vocabulary had never seen wrote an assignment whose
/// id nothing named — an id no fact resolves is an id nobody can render.
/// `Instance::assign_genre` got this right; the apply path bypassed it.
fn into_facts(proposal: Proposal, library: &impl Holdings) -> Result<Vec<Fact>, String> {
    Ok(match proposal {
        Proposal::Measured {
            encoding,
            cutoff_hz,
            analyser,
        } => vec![Fact::EncodingMeasured {
            encoding,
            cutoff_hz,
            analyser,
        }],
        // The genre is named rather than identified, because a tool has no way
        // to mint a concept id and should not: the vocabulary is ours (§2), so
        // the name is resolved against it here.
        // **All four fields cross now.** This line used to read
        // `{ recording, genre, .. } => Fact::GenreAssigned { recording, genre }`,
        // and that `..` threw away the basis and the confidence the tool had
        // gone to the trouble of establishing — so a Discogs guess and a typed
        // answer landed in the log as the same fact.
        // **Both concepts are catalogued before the edge**, the rule
        // `Instance::narrows` follows and for the same reason: an edge naming
        // an id no fact resolves is an ancestor nothing can put a name on.
        Proposal::Narrowed { broader, narrower } => {
            let facet = Id::<Facet>::of_key(facet::GENRE);
            let concept = |label: &str| Id::<Concept>::in_facet(facet::GENRE, label);
            if broader.trim().is_empty() || narrower.trim().is_empty() {
                return Err("a hierarchy edge needs two terms".to_string());
            }
            // **A term is not narrower than itself.** A self-edge is a cycle
            // of one, and the walk would spend its whole bound on it.
            //
            // Compared by **id, not by spelling**: `in_facet` normalises, so
            // `Non-Music` and `Non-music` are one concept and a string test
            // waves the edge through. It did — that self-edge reached this
            // library's log before the check was written this way.
            if concept(&broader) == concept(&narrower) {
                return Err(format!("{broader:?} cannot be narrower than itself"));
            }
            vec![
                Fact::ConceptCatalogued {
                    facet,
                    concept: concept(&broader),
                    label: broader.trim().to_string(),
                    node_label: false,
                },
                Fact::ConceptCatalogued {
                    facet,
                    concept: concept(&narrower),
                    label: narrower.trim().to_string(),
                    node_label: false,
                },
                Fact::HierarchyNarrowed {
                    hierarchy: Id::<Facet>::array(facet::GENRE),
                    parent: concept(&broader),
                    child: concept(&narrower),
                },
            ]
        }
        // Nothing is catalogued here: withdrawing an edge says nothing about
        // whether either concept should still exist, and a retraction that
        // quietly re-declared them would be doing two things.
        Proposal::NarrowingRetracted { broader, narrower } => {
            if broader.trim().is_empty() || narrower.trim().is_empty() {
                return Err("a hierarchy edge needs two terms".to_string());
            }
            vec![Fact::HierarchyNarrowRetracted {
                hierarchy: Id::<Facet>::array(facet::GENRE),
                parent: Id::<Concept>::in_facet(facet::GENRE, &broader),
                child: Id::<Concept>::in_facet(facet::GENRE, &narrower),
            }]
        }
        // **Both ends must be held**, because the claim is that `to` can be
        // remade from `from` — and a source this library does not have is a
        // promise it cannot keep.
        Proposal::Derived { from, to } => {
            if from == to {
                return Err("a file is not derived from itself".to_string());
            }
            if !library.holds(Subject::Encoding(from)) {
                return Err(format!(
                    "nothing here holds {from}, so nothing can be remade from it"
                ));
            }
            vec![Fact::EncodingDerived { from, to }]
        }
        Proposal::GenreAssigned {
            recording,
            genre,
            basis,
            confidence,
        } => {
            let concept = Id::<Concept>::in_facet(facet::GENRE, &genre);
            vec![
                Fact::ConceptCatalogued {
                    facet: Id::<Facet>::of_key(facet::GENRE),
                    concept,
                    label: genre.trim().to_string(),
                    node_label: false,
                },
                Fact::ConceptAsserted {
                    subject: Described::Recording(recording),
                    concept,
                    basis,
                    confidence,
                },
            ]
        }
        Proposal::Merged { loser, winner } => {
            // **A row cannot absorb itself.** Folding that to a no-op would
            // hide the usual cause: a pass that mapped both sides of a pair
            // through one lookup and got one answer.
            if loser == winner {
                return Err("a recording cannot be merged into itself".to_string());
            }
            vec![Fact::RecordingMerged { loser, winner }]
        }
        // The collection is named rather than identified, for the reason a
        // genre is: a tool has no way to mint an id and should not.
        Proposal::Credited {
            recording,
            artists,
            basis,
        } => {
            let named: Vec<String> = artists.iter().map(|a| a.trim().to_string()).collect();
            if named.iter().any(String::is_empty) {
                return Err("an artist needs a name".to_string());
            }
            // **Two names, or it is not a split.** A one-artist "split" would
            // quietly re-credit the recording to a differently-spelled artist,
            // which is a rename wearing a repair's clothes.
            if named.len() < 2 {
                return Err(format!(
                    "a credit names several artists, and this names {}",
                    named.len()
                ));
            }
            // **Positions are what keep them distinct**, so a duplicate name
            // would fold two credits into one row and lose the second.
            let mut seen = std::collections::BTreeSet::new();
            if let Some(twice) = named
                .iter()
                .find(|a| !seen.insert(crate::model::ingest::normalise(a)))
            {
                return Err(format!("{twice:?} is credited twice"));
            }
            let role = Id::<Role>::of_name("performer");
            let mut facts = vec![Fact::RoleDefined {
                role,
                name: "performer".to_string(),
                class: RoleClass::Performer,
            }];
            for (at, name) in named.iter().enumerate() {
                let artist = Id::<Artist>::of_artist(None, name);
                facts.push(Fact::ArtistNamed {
                    artist,
                    name: name.clone(),
                });
                facts.push(Fact::CreditAsserted {
                    subject: CreditOn::Recording(recording),
                    artist,
                    role,
                    // **Whatever the proposer says**, and never `TagVariant`:
                    // the row this supersedes was a tag's reading, and folding
                    // them to one basis would make the correction
                    // indistinguishable from the thing it corrects.
                    basis,
                    position: u8::try_from(at).unwrap_or(u8::MAX),
                    as_credited: None,
                    join_phrase: None,
                });
            }
            facts
        }
        Proposal::CollectedInto {
            recording,
            collection,
            ordered,
        } => {
            let name = collection.trim();
            if name.is_empty() {
                return Err("a collection needs a name".to_string());
            }
            let id = Id::<Collection>::of_name(name);
            vec![
                Fact::CollectionNamed {
                    collection: id,
                    name: name.to_string(),
                    ordered,
                },
                Fact::CollectionEntryAdded {
                    collection: id,
                    recording,
                    // **Every seeded entry shares a position, and that is the
                    // honest encoding.** A folder has no running order, so
                    // giving each entry a distinct key would invent one — and
                    // an arbitrary order that looks meaningful is worse than
                    // none. Entries stay distinct because the fold keys them
                    // by `(collection, position, recording)`, and applying the
                    // same proposal twice is the same row.
                    //
                    // A collection later made *ordered* wants its keys spread
                    // first, which is the compaction `domain-model.md` names.
                    ord: OrdKey::first(),
                },
            ]
        }
        // Refused at the door rather than folded to nothing, for the reason
        // `Instance::assign_qualifier` refuses it: a grouping concept is a
        // place in the tree, not a claim about a song, and an assignment
        // nobody can see is indistinguishable from one nobody made.
        Proposal::QualifierAssigned {
            recording,
            qualifier,
            basis,
            confidence,
        } => {
            if !crate::model::qualifier::assignable(&qualifier) {
                return Err(format!(
                    "{qualifier:?} is a node label and says nothing about a recording"
                ));
            }
            let concept = Id::<Concept>::in_facet(facet::VERSION, &qualifier);
            vec![
                Fact::ConceptCatalogued {
                    facet: Id::<Facet>::of_key(facet::VERSION),
                    concept,
                    label: qualifier.trim().to_string(),
                    node_label: false,
                },
                Fact::ConceptAsserted {
                    subject: Described::Recording(recording),
                    concept,
                    basis,
                    confidence,
                },
            ]
        }
        // **A relink, and the one proposal that is not a function of its own
        // line.** Where the binding moves *from* is a fact about the library
        // now, not about what the tool found, so `parse` has to be able to ask.
        Proposal::Identified { encoding, basis } => {
            let to = Id::<Recording>::of_basis(&basis);
            let from = library.bound(encoding);
            if from == Some(to) {
                return Err("already holds that performance".into());
            }
            let mut facts = vec![
                Fact::RecordingIdentified {
                    recording: to,
                    basis,
                },
                Fact::EncodingBound { encoding, from, to },
            ];
            // **A held-back file is catalogued here, from what it claimed** —
            // #160. The gate keeps a recording from being minted out of a
            // file's own bytes, so nothing named this performance and nothing
            // gave it a title; without this the identification would bind an
            // encoding to a recording the library cannot draw.
            //
            // **From the claim rather than from the file.** The tags were
            // right about the title even when they carried no id, and that is
            // already in the log — re-opening the file to learn it again would
            // be a second door onto one piece of evidence, and `apply` does
            // not read audio.
            if from.is_none()
                && let Some((artist, title)) = library.claim(encoding)
            {
                let who = Id::<Artist>::of_artist(None, &artist);
                facts.push(Fact::ArtistNamed {
                    artist: who,
                    name: artist,
                });
                facts.push(Fact::RecordingCatalogued {
                    recording: to,
                    title,
                    artist: who,
                });
            }
            // **The appearance is re-pointed too** — #155. Two facts name a
            // recording: the file says which performance it holds, and the
            // slot it fills says which performance sits there. Moving only the
            // first left the slot naming the song the file used to be, so a
            // rating given to the row went to the old song — which is exactly
            // what a relink is undoing.
            //
            // If the file was misidentified, so was the place it sits.
            if let Some(slot) = library.slot(encoding) {
                facts.push(Fact::TrackPlaced {
                    track: slot.track,
                    medium: slot.medium,
                    recording: to,
                    position: slot.position,
                    // Not carried: what the sleeve printed is a fact about the
                    // release, and this proposal says nothing about it.
                    title_on_release: None,
                });
            }
            // **The history has to come too.**
            //
            // Naming a performance mints a new id, and every rating and play
            // anyone gave the old one hangs off the id nothing points at now.
            // Applying 377 identifications stranded 7 of 11 ratings and 118 of
            // 255 played recordings exactly that way — nothing failed, the
            // facts were all still in the log, and they were unreachable.
            //
            // **Only when the old recording was minted from the bytes.** That
            // rung means "nothing had named this", so the two are one
            // performance badly named rather than two performances — and
            // merging is a repair rather than a claim. A recording an
            // authority had already named is a different performance, and
            // pointing a file at it is a relink with no merge in it.
            if let Some(old) = from
                && matches!(library.identified(old), Some(Basis::Audio(_)))
            {
                facts.push(Fact::RecordingMerged {
                    loser: old,
                    winner: to,
                });
            }
            facts
        }
        // **The tracklist becomes slots** — #155, and the half that makes the
        // greying visible. Every fact here is derived from the ids the tool
        // sent; nothing is invented and nothing is looked up.
        Proposal::Tracklisted {
            release,
            position,
            format,
            tracks,
        } => {
            if tracks.is_empty() {
                return Err("a tracklist with no tracks says nothing".into());
            }
            // **Refused rather than deduplicated**, because the slot id is a
            // function of the recording: two rows naming one recording fold to
            // one slot, so the record would silently come out shorter than the
            // list that was applied. A record that really plays one recording
            // twice is `of_track`'s known cost and wants a decision, not a
            // quiet halving of the denominator.
            let mut seen = BTreeSet::new();
            if let Some(twice) = tracks.iter().find(|t| !seen.insert(t.recording)) {
                return Err(format!("{} is listed twice", twice.recording));
            }
            let release = Id::<Release>::of_musicbrainz(release);
            let medium = Id::<Medium>::of_medium(release, position);
            let role = Id::<Role>::of_name("performer");
            let mut facts = vec![
                // **The denominator, and the whole medium re-stated** — the
                // fold takes this row wholesale, so a field left out is a
                // field cleared. #134 put `tracks` here and nothing had ever
                // been in a position to fill it from an authority.
                Fact::MediumCatalogued {
                    medium,
                    release,
                    position,
                    format,
                    tracks: u16::try_from(tracks.len()).ok(),
                },
                Fact::RoleDefined {
                    role,
                    name: "performer".to_string(),
                    class: RoleClass::Performer,
                },
            ];
            if let Some(nameless) = tracks.iter().find(|t| t.artists.is_empty()) {
                return Err(format!("{} is credited to nobody", nameless.recording));
            }
            for listed in tracks {
                let recording = Id::<Recording>::of_basis(&Basis::MusicBrainz(listed.recording));
                facts.push(Fact::RecordingIdentified {
                    recording,
                    basis: Basis::MusicBrainz(listed.recording),
                });
                for (at, name) in listed.artists.iter().enumerate() {
                    let artist = Id::<Artist>::of_artist(None, name);
                    facts.push(Fact::ArtistNamed {
                        artist,
                        name: name.clone(),
                    });
                    facts.push(Fact::CreditAsserted {
                        subject: CreditOn::Recording(recording),
                        artist,
                        role,
                        basis: Source::MusicBrainz,
                        position: u8::try_from(at).unwrap_or(u8::MAX),
                        as_credited: None,
                        join_phrase: None,
                    });
                }
                facts.push(Fact::RecordingCatalogued {
                    recording,
                    title: listed.title.clone(),
                    // **The first, and the field's own doc says why that is
                    // enough**: it can name one artist and nothing about them,
                    // and it survives only until the tree reads credits
                    // instead. The credits above are the answer that is right.
                    artist: Id::<Artist>::of_artist(None, &listed.artists[0]),
                });
                facts.push(Fact::TrackPlaced {
                    track: Id::<Track>::of_track(medium, recording),
                    medium,
                    recording,
                    position: listed.position,
                    // **What the sleeve prints, where the tool found a real
                    // difference.** `None` on nearly every row: the tool
                    // compares the release's title against the recording's and
                    // sends this only when they differ by more than
                    // punctuation, so a normalised dash does not read as a
                    // rename.
                    title_on_release: listed.title_on_release.clone(),
                });
            }
            facts
        }
        Proposal::Named { artist, name } => {
            let name = name.trim();
            if name.is_empty() {
                // **A name nobody can read is worse than none**, because the
                // absence is at least visible: `line_of` drops a credit whose
                // artist has no name, and an empty label would print a
                // separator introducing nobody.
                return Err("an artist needs a name".into());
            }
            vec![Fact::ArtistNamed {
                artist,
                name: name.to_string(),
            }]
        }
        Proposal::Retitled { recording, title } => {
            let title = title.trim();
            if title.is_empty() {
                // **A row with no title cannot be found or played**, and unlike
                // a missing artist name there is nothing else to draw it by.
                return Err("a recording needs a title".into());
            }
            vec![Fact::RecordingRetitled {
                recording,
                title: title.to_string(),
            }]
        }
        Proposal::Contributed {
            recording,
            artist,
            role,
            basis,
        } => {
            let name = artist.trim();
            if name.is_empty() {
                return Err("an artist needs a name".into());
            }
            let wanted = role.trim().to_lowercase();
            // **The class is what makes this refusable.** `RoleDefined` takes
            // any name, but it takes a class with it and a string does not say
            // which — and a `remixer` classed as `Performer` reads back as
            // somebody the record is *by*.
            let Some((canonical, class)) = crate::model::reference::ROLES
                .iter()
                .find(|(known, _)| *known == wanted)
            else {
                let known: Vec<&str> = crate::model::reference::ROLES
                    .iter()
                    .map(|(name, _)| *name)
                    .collect();
                return Err(format!(
                    "{role:?} is not a role this codelist knows — {}",
                    known.join(", ")
                ));
            };
            let role_id = Id::<Role>::of_name(canonical);
            let who = Id::<Artist>::of_artist(None, name);
            vec![
                Fact::RoleDefined {
                    role: role_id,
                    name: (*canonical).to_string(),
                    class: *class,
                },
                Fact::ArtistNamed {
                    artist: who,
                    name: name.to_string(),
                },
                Fact::CreditAsserted {
                    subject: CreditOn::Recording(recording),
                    artist: who,
                    role: role_id,
                    basis,
                    // **Zero, and it is cosmetic here.** A position orders a
                    // credit *line*, and `line_of` reads performers only — a
                    // remixer never appears in one. Two remixers stay distinct
                    // because the fold keys on the artist.
                    position: 0,
                    as_credited: None,
                    join_phrase: None,
                },
            ]
        }
        Proposal::RecordingDerived { from, to, kind } => {
            let origin = Id::<Recording>::of_basis(&from);
            if origin == to {
                return Err("a recording cannot be derived from itself".into());
            }
            vec![
                // **The far end is named even though nothing holds it.** One
                // fact, and it is what turns a bare id into something an info
                // panel can say out loud — "made out of the recording
                // MusicBrainz calls X" — without inventing a title it would
                // then have to draw.
                Fact::RecordingIdentified {
                    recording: origin,
                    basis: from,
                },
                Fact::RecordingDerived {
                    from: origin,
                    to,
                    kind,
                },
            ]
        }
        Proposal::GroupsMerged { loser, winner } => {
            if loser == winner {
                return Err("an album cannot absorb itself".into());
            }
            vec![Fact::ReleaseGroupMerged { loser, winner }]
        }
        Proposal::ArtistsMerged { loser, winner } => {
            if loser == winner {
                return Err("an artist cannot absorb themselves".into());
            }
            vec![Fact::ArtistMerged { loser, winner }]
        }
        Proposal::CreditWithdrawn {
            subject,
            artist,
            role,
            basis,
        } => {
            let wanted = role.trim().to_lowercase();
            // **The same codelist the assertion used.** A withdrawal naming a
            // role nothing was ever credited under would shadow nothing and
            // report success, which is the quiet failure this refuses.
            let Some((canonical, _)) = crate::model::reference::ROLES
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&wanted))
            else {
                let known: Vec<_> = crate::model::reference::ROLES
                    .iter()
                    .map(|(name, _)| *name)
                    .collect();
                return Err(format!(
                    "{role:?} is not a role this codelist knows — {}",
                    known.join(", ")
                ));
            };
            vec![Fact::CreditRetracted {
                subject,
                artist,
                role: Id::<Role>::of_name(canonical),
                basis,
            }]
        }
        Proposal::Rated { recording, stars } => {
            vec![Fact::RecordingRated { recording, stars }]
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn dev() -> Id<Device> {
        Id::derived(b"laptop")
    }

    fn enc(n: u128) -> Id<Encoding> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    pub(crate) fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// A recording MusicBrainz has named, which the double holds — so a
    /// proposal about the *recording* and one about the record it is on can
    /// be compared against the same library.
    pub(crate) fn named() -> uuid::Uuid {
        uuid::Uuid::from_u128(0x51)
    }

    /// An artist the double credits and has never named — #154's case.
    pub(crate) fn nameless() -> Id<Artist> {
        Id::derived(b"identified and never named")
    }

    /// The one album the double holds.
    pub(crate) fn album() -> Id<ReleaseGroup> {
        Id::derived(b"an album the library has")
    }

    /// The MusicBrainz release the double holds one disc of.
    pub(crate) fn pressed() -> uuid::Uuid {
        uuid::Uuid::from_u128(0xbbbb)
    }

    fn pressing() -> Id<Release> {
        Id::<Release>::of_musicbrainz(pressed())
    }

    /// The MusicBrainz id encoding 3 has already been identified as, so that
    /// "propose what is already true" is reachable rather than hypothetical.
    fn settled() -> uuid::Uuid {
        uuid::Uuid::from_u128(0xaaaa)
    }

    /// A library holding encodings 1–3 and recordings 1 and 2. Encoding 1 is
    /// bound to a recording minted from its bytes, encoding 2 is unbound, and
    /// encoding 3 already holds the performance `settled()` names.
    pub(crate) struct Held;

    impl Holdings for Held {
        fn holds(&self, s: Subject) -> bool {
            match s {
                // `enc(4)` is the gate's case (#160): bytes the library holds
                // and has bound to no recording, carrying only a claim.
                Subject::Encoding(e) => e == enc(1) || e == enc(2) || e == enc(3) || e == enc(4),
                Subject::Recording(r) => {
                    r == rec(1) || r == rec(2) || r == Id::of_basis(&Basis::MusicBrainz(named()))
                }
                Subject::Vocabulary => true,
                // **Disc 1 of `pressing()`, and nothing else.** A tracklist is
                // refused for a record the library has never seen, and that
                // refusal is the safety property worth having a double for.
                Subject::Medium(m) => m == Id::<Medium>::of_medium(pressing(), 1),
                // The double holds one album, so a merge into it is reachable
                // and a merge into anything else is stale.
                Subject::ReleaseGroup(g) => g == album(),
                // Held, and deliberately nameless: the case #154 exists for
                // is an artist with a right id and no name.
                Subject::Artist(a) => a == nameless(),
            }
        }

        fn bound(&self, encoding: Id<Encoding>) -> Option<Id<Recording>> {
            // `enc(4)` is bound to nothing on purpose — see `claim`.
            match encoding {
                e if e == enc(1) => Some(rec(1)),
                e if e == enc(3) => Some(Id::of_basis(&Basis::MusicBrainz(settled()))),
                _ => None,
            }
        }

        /// `rec(1)` rests on its bytes — nothing had named it — and the
        /// recording encoding 3 holds was named by MusicBrainz.
        /// **The test double holds no releases**, so nothing it holds fills a
        /// slot. A relink against it re-points a binding and nothing else,
        /// which is what the `credited`/`merged` tests want; the slot half is
        /// exercised where there is a spine to have one.
        fn slot(&self, _encoding: Id<Encoding>) -> Option<Slot> {
            None
        }

        /// **The row the gate held back**, so an identification against bytes
        /// nobody named can be exercised against the real `apply`.
        fn claim(&self, encoding: Id<Encoding>) -> Option<(String, String)> {
            (encoding == enc(4)).then(|| ("Roberto Capuano".into(), "Till Dawn".into()))
        }

        fn identified(&self, recording: Id<Recording>) -> Option<Basis> {
            if recording == rec(1) {
                Some(Basis::Audio(crate::audio_id::AudioId {
                    scheme: 1,
                    hash: 1,
                }))
            } else {
                Some(Basis::MusicBrainz(settled()))
            }
        }
    }

    pub(crate) const HELD: &Held = &Held;

    fn measured(e: u128, hz: u32) -> String {
        format!(
            r#"{{"fact":"measured","encoding":"{}","cutoff_hz":{hz},"analyser":1}}"#,
            enc(e)
        )
    }

    /// The shape `quality.py` writes: a measurement about **an encoding**,
    /// because a cutoff is a fact about particular bytes.
    #[test]
    fn a_measurement_becomes_a_fact() {
        let (records, report) = parse(&measured(1, 15_700), dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        assert_eq!(
            records.iter().next().unwrap().fact,
            Fact::EncodingMeasured {
                encoding: enc(1),
                cutoff_hz: 15_700,
                analyser: 1,
            }
        );
    }

    /// **`basis` and `confidence` are on the wire.** A Discogs guess and a
    /// typed answer are the same shape, and a format that could not tell them
    /// apart would throw away the only thing that makes a review queue
    /// possible.
    #[test]
    fn a_genre_carries_where_it_came_from() {
        let good = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno","basis":"discogs","confidence":"matched"}}"#,
            rec(1)
        );
        let (_, report) = parse(&good, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let bare = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno"}}"#,
            rec(1)
        );
        let (_, report) = parse(&bare, dev(), 100, HELD);
        assert_eq!(
            report.malformed.len(),
            1,
            "a genre with no stated basis is not a fact this library will take"
        );
    }

    /// **A hierarchy edge is curation, not a claim about the disk** — #79.
    ///
    /// Both concepts are catalogued before the edge, so nothing names an id no
    /// fact resolves. And it is gated on nothing the library holds: a term the
    /// hierarchy is being built ahead of is exactly the one nothing is tagged
    /// with yet.
    #[test]
    fn a_hierarchy_edge_catalogues_both_terms_and_needs_no_holding() {
        let line = r#"{"fact":"narrowed","broader":"Electronic","narrower":"Techno"}"#;
        let (records, report) = parse(line, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let facts: Vec<_> = records.iter().map(|r| r.fact.clone()).collect();
        assert_eq!(facts.len(), 3, "{facts:?}");
        assert!(
            matches!(&facts[2], Fact::HierarchyNarrowed { .. }),
            "the edge is written last, after what it names: {facts:?}"
        );
        assert!(
            facts[..2]
                .iter()
                .all(|f| matches!(f, Fact::ConceptCatalogued { .. })),
            "an edge named a concept nothing catalogued: {facts:?}"
        );
    }

    /// **A term is not narrower than itself**, and an empty one names nothing.
    ///
    /// A self-edge is a cycle of one: the bounded walk would spend its entire
    /// **A transcode is a fact the maker declares** — #132, and the only
    /// route to it: the bytes do not say where they came from, and a format
    /// does not either.
    #[test]
    fn a_derived_copy_names_the_file_it_was_made_from() {
        let line = r#"{"fact":"derived","from":"00000000-0000-0000-0000-000000000001","to":"00000000-0000-0000-0000-000000000002"}"#;
        let (records, report) = parse(line, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");
        let facts: Vec<_> = records.iter().map(|r| r.fact.clone()).collect();
        assert!(
            matches!(&facts[..], [Fact::EncodingDerived { from, to }] if *from == enc(1) && *to == enc(2)),
            "{facts:?}"
        );
    }

    /// **A source this library does not hold is a promise it cannot keep.**
    ///
    /// The edge means "`to` can be made again from `from`". If `from` is not
    /// here, it cannot, and an edge saying otherwise would make a file that
    /// needs backing up look disposable — the one direction in which being
    /// wrong loses music.
    #[test]
    fn a_derivation_refuses_a_source_nothing_holds() {
        let unheld = uuid::Uuid::from_u128(9);
        let line = format!(
            r#"{{"fact":"derived","from":"{unheld}","to":"00000000-0000-0000-0000-000000000002"}}"#
        );
        let (records, report) = parse(&line, dev(), 100, HELD);
        assert_eq!(report.applied, 0, "{report}");
        assert!(records.is_empty());
        assert_eq!(report.refused.len(), 1, "{report}");

        // And the other end: a derived file nothing here holds is not a
        // subject this library has heard of, which is `unknown` rather than
        // refused — the same answer every other proposal gives.
        let line = format!(
            r#"{{"fact":"derived","from":"00000000-0000-0000-0000-000000000001","to":"{unheld}"}}"#
        );
        let (_, report) = parse(&line, dev(), 100, HELD);
        assert_eq!(report.applied, 0, "{report}");
        assert_eq!(
            report.unknown,
            vec![Subject::Encoding(Id::from_uuid(unheld))]
        );
    }

    /// A file is not made out of itself, and an edge saying so would make it
    /// its own reason for being disposable.
    #[test]
    fn a_derivation_refuses_a_loop_of_one() {
        let line = r#"{"fact":"derived","from":"00000000-0000-0000-0000-000000000002","to":"00000000-0000-0000-0000-000000000002"}"#;
        let (_, report) = parse(line, dev(), 100, HELD);
        assert_eq!(report.applied, 0, "{report}");
    }

    /// budget on it, and `genre@1` would climb to where it started.
    #[test]
    fn a_hierarchy_edge_refuses_a_loop_of_one() {
        for bad in [
            r#"{"fact":"narrowed","broader":"Techno","narrower":"Techno"}"#,
            // **Two spellings of one concept.** `in_facet` normalises, so this
            // is the same self-edge wearing different capitals — and a string
            // comparison waves it through.
            r#"{"fact":"narrowed","broader":"Non-Music","narrower":"non-music"}"#,
            r#"{"fact":"narrowed","broader":"","narrower":"Techno"}"#,
        ] {
            let (_, report) = parse(bad, dev(), 100, HELD);
            assert_eq!(report.applied, 0, "{bad} was applied");
        }
    }

    /// **A subject the library does not hold is reported, never invented.**
    ///
    /// A proposal written against a library that has since changed is *stale*
    /// rather than wrong, and naming what it missed is what lets a tool
    /// re-query instead of re-measuring.
    #[test]
    fn an_unknown_subject_is_named_not_invented() {
        let text = format!("{}\n{}", measured(1, 100), measured(99, 200));
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(records.len(), 1);
        assert_eq!(report.unknown, vec![Subject::Encoding(enc(99))]);
        assert!(report.balances(2), "{report}");
    }

    /// **A path is not an identifier**, and the format will not take one.
    ///
    /// This is the whole point of the change: a path is a property of one
    /// device's copy, stale the moment a file moves and meaningless on another
    /// machine.
    #[test]
    fn a_proposal_naming_a_path_is_refused() {
        let text = r#"{"fact":"measured","path":"/m/a.mp3","cutoff_hz":15700,"analyser":1}"#;
        let (records, report) = parse(text, dev(), 100, HELD);

        assert!(records.is_empty());
        assert_eq!(report.malformed.len(), 1);
    }

    /// A typo must not cost a whole pass, but it must be findable — so the
    /// line number comes back with it.
    #[test]
    fn a_bad_line_is_reported_with_its_number_and_the_rest_still_apply() {
        let text = format!("{}\n{{not json at all\n{}", measured(1, 1), measured(2, 2));
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 2);
        assert_eq!(report.malformed.len(), 1);
        assert_eq!(report.malformed[0].0, 2, "the line a person would jump to");
        assert_eq!(records.len(), 2);
    }

    /// A reviewed file is one somebody has been editing, and both of these are
    /// what people write while editing.
    #[test]
    fn blank_lines_and_comments_are_not_errors() {
        let text = format!("# measured 2026-09-02\n\n{}\n\n", measured(1, 1));
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1);
        assert!(report.malformed.is_empty());
        assert_eq!(records.len(), 1);
    }

    /// Two measurements taken in the same second must not collapse into one.
    #[test]
    fn proposals_in_one_file_do_not_collide() {
        let text = format!("{}\n{}", measured(1, 1), measured(2, 2));
        let (records, _) = parse(&text, dev(), 100, HELD);
        assert_eq!(records.len(), 2);
    }

    /// **A concept has to exist before it can be assigned.**
    ///
    /// The apply path used to emit the assignment alone, so a genre the
    /// vocabulary had never seen arrived as an id no fact resolved — an id
    /// nobody can render, and a row that looks like a bug in the renderer
    /// rather than a gap in the log. `Instance::assign_genre` always got this
    /// right; this path bypassed it.
    #[test]
    fn an_assignment_brings_its_concept_with_it() {
        let text = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Dub Techno","basis":"discogs","confidence":"matched"}}"#,
            rec(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "one line, however many facts");
        assert!(
            records.iter().any(
                |r| matches!(&r.fact, Fact::ConceptCatalogued { label, .. } if label == "Dub Techno")
            ),
            "the assignment names a concept nothing defines"
        );
        assert_eq!(records.len(), 2);
    }

    /// **The wire form is what a person would type.**
    ///
    /// Snake case turned `MusicBrainz` into `music_brainz`, which nobody has
    /// ever written down. Found by writing a test against the format the way a
    /// person would use it, which is the only way this kind of defect shows
    /// up — every round trip through serde agreed with itself.
    #[test]
    fn a_basis_is_spelled_the_way_it_is_said() {
        for (wire, basis) in [
            ("musicbrainz", Source::MusicBrainz),
            ("discogs", Source::Discogs),
            ("manual", Source::Manual),
            ("tag_variant", Source::TagVariant),
        ] {
            let text = format!(
                r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno","basis":"{wire}","confidence":"matched"}}"#,
                rec(1)
            );
            let (records, report) = parse(&text, dev(), 100, HELD);
            assert!(report.malformed.is_empty(), "{wire}: {report}");
            assert!(records.iter().any(|r| matches!(
                &r.fact,
                Fact::ConceptAsserted { basis: b, .. } if *b == basis
            )));
        }
    }

    /// A qualifier proposal is the route that replaces a stopgap: the matcher
    /// used to *downgrade* a match whose title carried a version it had not
    /// asked for, because there was nowhere to record what it had noticed.
    #[test]
    fn a_qualifier_can_be_proposed() {
        let text = format!(
            r#"{{"fact":"qualifier_assigned","recording":"{}","qualifier":"Instrumental","basis":"musicbrainz","confidence":"matched"}}"#,
            rec(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        assert!(report.refused.is_empty(), "{report}");
        assert!(records.iter().any(|r| matches!(
            &r.fact,
            Fact::ConceptAsserted { concept, .. }
                if *concept == Id::<Concept>::in_facet(facet::VERSION, "instrumental")
        )));
    }

    /// **A grouping concept is refused, and refusal is its own outcome.**
    ///
    /// The line parsed and the library holds the recording — what is wrong is
    /// the claim, so calling it malformed would send a person looking for a
    /// typo. `mix-variant` is a place in the tree rather than a statement
    /// about a song, and folding it would make an assignment nobody can see.
    #[test]
    fn a_grouping_concept_is_refused_with_its_reason() {
        let text = format!(
            r#"{{"fact":"qualifier_assigned","recording":"{}","qualifier":"mix-variant","basis":"manual","confidence":"confirmed"}}"#,
            rec(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert!(records.is_empty());
        assert!(report.malformed.is_empty(), "a refusal is not a typo");
        assert_eq!(report.refused.len(), 1);
        assert_eq!(report.refused[0].0, 1, "the line a person would jump to");
        assert!(report.balances(1), "{report}");
    }

    /// **An identification is a relink**, and the binding says what it moved
    /// from.
    ///
    /// The encoding is the subject because the recording's id *is* the claim
    /// being corrected — a recording nothing had named was minted from these
    /// bytes, so naming it would mean naming the thing this replaces.
    #[test]
    fn an_identification_moves_the_binding() {
        let mbid = uuid::Uuid::from_u128(0xca11_1500);
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{mbid}"}}}}"#,
            enc(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        assert!(
            records.iter().any(|r| matches!(
                &r.fact,
                Fact::EncodingBound { encoding, from, to }
                    if *encoding == enc(1) && *from == Some(rec(1)) && *to == named
            )),
            "the binding did not move, or lost what it replaced"
        );
        assert!(records.iter().any(|r| matches!(
            &r.fact,
            Fact::RecordingIdentified { basis: Basis::MusicBrainz(u), .. } if *u == mbid
        )));
    }

    /// **A description does not ride along.** An identification says which
    /// performance this is; what it is called is a separate claim somebody
    /// else can propose, and inventing one here would be the projection's old
    /// habit in a new place.
    #[test]
    fn an_identification_does_not_rename_anything() {
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{}"}}}}"#,
            enc(1),
            uuid::Uuid::from_u128(7)
        );
        let (records, _) = parse(&text, dev(), 100, HELD);
        assert!(
            !records
                .iter()
                .any(|r| matches!(&r.fact, Fact::RecordingCatalogued { .. })),
            "a title arrived from somewhere nobody proposed one"
        );
    }

    /// **Re-running a reviewed file relinks nothing.**
    ///
    /// Refused rather than applied and then deduplicated: the second run has
    /// nothing to say, and an `EncodingBound { from: X, to: X }` in the log
    /// would read as a decision somebody made.
    #[test]
    fn identifying_what_is_already_identified_is_refused() {
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{}"}}}}"#,
            enc(3),
            settled()
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert!(records.is_empty());
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
        assert!(report.balances(1), "{report}");
    }

    /// **The history comes with it**, which is the whole point of merging.
    ///
    /// Naming a performance mints a new id. Without this, every rating and
    /// play given to the byte-minted one hangs off an id nothing points at:
    /// 7 of 11 ratings and 118 of 255 played recordings, on the first real
    /// run, all still in the log and none of them reachable.
    #[test]
    fn identifying_a_byte_minted_recording_merges_it() {
        let mbid = uuid::Uuid::from_u128(0xca11_1500);
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{mbid}"}}}}"#,
            enc(1)
        );
        let (records, _) = parse(&text, dev(), 100, HELD);

        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        assert!(
            records.iter().any(|r| matches!(
                &r.fact,
                Fact::RecordingMerged { loser, winner } if *loser == rec(1) && *winner == named
            )),
            "the ratings and plays were left on the id nothing points at"
        );
    }

    /// **A recording an authority already named is not merged away.**
    ///
    /// Then the two are different performances and pointing a file at one of
    /// them is an ordinary relink. Merging would be a claim rather than a
    /// repair, and a false merge is the error this model refuses to make —
    /// a split is one merge from repaired and a merge is permanent.
    #[test]
    fn relinking_away_from_a_named_recording_does_not_merge() {
        // Encoding 3 holds a recording MusicBrainz named; point it elsewhere.
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{}"}}}}"#,
            enc(3),
            uuid::Uuid::from_u128(0xbbbb)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        assert!(
            !records
                .iter()
                .any(|r| matches!(&r.fact, Fact::RecordingMerged { .. })),
            "a relink between two named performances merged them"
        );
    }

    /// The old spelling still reads, because it is already in the live log.
    #[test]
    fn a_basis_written_the_old_way_still_parses() {
        let mbid = uuid::Uuid::from_u128(9);
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"MusicBrainz":"{mbid}"}}}}"#,
            enc(2)
        );
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert!(report.malformed.is_empty(), "{report}");
        assert_eq!(report.applied, 1);
    }

    /// **The round trip.** A tool is handed an id and a locator, opens the
    /// locator, and proposes against the id — so it never computes an identity
    /// and never keys anything by a path.
    #[test]
    fn a_tool_proposes_against_what_the_query_handed_it() {
        let row = Queried {
            recording: Some(rec(1)),
            encoding: Some(enc(1)),
            locator: Some("/m/a.mp3".into()),
            length_ms: Some(320_000),
            title: "Callisto".into(),
            artist: "Bodzin & Romboy".into(),
        };
        let wire = serde_json::to_string(&row).unwrap();
        let back: Queried = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, row, "the query output survives the wire");

        // The tool opens `locator`, measures, and names `encoding`.
        let proposal = format!(
            r#"{{"fact":"measured","encoding":"{}","cutoff_hz":19800,"analyser":1}}"#,
            back.encoding.unwrap()
        );
        let (records, report) = parse(&proposal, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");
        assert_eq!(
            records.iter().next().unwrap().fact,
            Fact::EncodingMeasured {
                encoding: enc(1),
                cutoff_hz: 19_800,
                analyser: 1,
            }
        );
    }

    /// A recording this machine does not hold still comes back from a query —
    /// decided about, not owned — with nothing to open.
    /// **Naming held-back bytes catalogues them from what they claimed** —
    /// #160, and the round trip the gate exists to make possible.
    ///
    /// The gate keeps a recording from being minted out of a file's own bytes,
    /// so a file it held back has no performance and no title. An
    /// identification that only bound the encoding would leave a recording the
    /// library cannot draw — a row with an id and nothing to print.
    ///
    /// The claim is promoted rather than re-read: the tags were right about
    /// the title even when they carried no id, and that is already in the log.
    #[test]
    fn identifying_held_back_bytes_catalogues_them_from_the_claim() {
        let mbid = uuid::Uuid::from_u128(0x7711);
        let p = Proposal::Identified {
            encoding: enc(4),
            basis: Basis::MusicBrainz(mbid),
        };
        let text = serde_json::to_string(&p).expect("serialises");
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let to = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        let facts: Vec<_> = records.iter().map(|r| &r.fact).collect();
        assert!(
            facts.iter().any(|f| matches!(
                f,
                Fact::EncodingBound {
                    encoding,
                    from: None,
                    to: bound
                } if *encoding == enc(4) && *bound == to
            )),
            "the encoding was not bound to the named recording"
        );
        assert!(
            facts.iter().any(|f| matches!(
                f,
                Fact::RecordingCatalogued { recording, title, .. }
                    if *recording == to && title == "Till Dawn"
            )),
            "the recording has an id and no title: {facts:?}"
        );
        // **No merge.** Merging is a repair for two ids naming one
        // performance, and held-back bytes never minted a first one — there is
        // nothing to redirect.
        assert!(
            !facts
                .iter()
                .any(|f| matches!(f, Fact::RecordingMerged { .. })),
            "a merge was written for a recording that never existed"
        );
    }

    #[test]
    fn a_row_with_no_copy_here_is_still_a_row() {
        let row = Queried {
            recording: Some(rec(2)),
            encoding: None,
            locator: None,
            length_ms: None,
            title: "Wanted".into(),
            artist: "Someone".into(),
        };
        let wire = serde_json::to_string(&row).unwrap();
        assert_eq!(serde_json::from_str::<Queried>(&wire).unwrap(), row);
    }

    /// An empty file is a successful no-op, not a failure. A pass that found
    /// nothing to say should not read as a pass that broke.
    #[test]
    fn nothing_to_say_is_not_an_error() {
        let (records, report) = parse("", dev(), 100, HELD);
        assert!(records.is_empty() && report.applied == 0);
        assert!(report.malformed.is_empty() && report.unknown.is_empty());
    }
}

#[cfg(test)]
mod collecting {
    //! **A path-based catalog becoming a curated set** — #94.
    //!
    //! Measured on the real library: no genre rule reproduces
    //! `~/MusicLibrary/Dance`. Every one of its 790 files carries an
    //! `Electronic` term and so do 876 files in `General` — Laurie Anderson,
    //! Beck, Enya, DEVO, Britney Spears — which are correctly tagged and are
    //! simply not what the folder means. The best rule tried still misplaces
    //! 139 files; the obvious one more than doubles the catalog.
    //!
    //! What the folder records is a judgement rather than a genre, and a
    //! collection is where a judgement lives.

    use super::tests::*;
    use super::*;

    fn line(r: u128, into: &str) -> String {
        format!(
            r#"{{"fact":"collected_into","recording":"{}","collection":"{into}"}}"#,
            rec(r)
        )
    }

    /// The concept before the membership, for the reason a genre needs one: an
    /// id no fact resolves is an id nobody can render.
    #[test]
    fn a_collection_is_named_before_anything_is_put_in_it() {
        let (records, report) = parse(&line(1, "Dance"), dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let facts: Vec<&Fact> = records.iter().map(|r| &r.fact).collect();
        assert!(
            facts
                .iter()
                .any(|f| matches!(f, Fact::CollectionNamed { name, .. } if name == "Dance")),
            "the collection was never named: {facts:?}"
        );
        assert!(
            facts
                .iter()
                .any(|f| matches!(f, Fact::CollectionEntryAdded { .. })),
            "nothing was put in it: {facts:?}"
        );
    }

    /// **A concept is declared once per file, not once per line.**
    ///
    /// Every assignment brings its concept with it, because an id no fact
    /// resolves is an id nobody can render. But seeding the Dance collection
    /// from its folder was 761 proposals into one collection, and it put 761
    /// identical `CollectionNamed` records into a log of 7,974 — they fold to
    /// one row and cost a durable line each.
    #[test]
    fn one_collection_is_named_once_however_many_go_into_it() {
        let file = (1..=2)
            .map(|n| line(n, "Dance"))
            .collect::<Vec<_>>()
            .join("\n");
        let (records, report) = parse(&file, dev(), 100, HELD);
        assert_eq!(report.applied, 2, "{report}");

        let named = records
            .iter()
            .filter(|r| matches!(&r.fact, Fact::CollectionNamed { .. }))
            .count();
        assert_eq!(named, 1, "the collection was named once per entry");
        let added = records
            .iter()
            .filter(|r| matches!(&r.fact, Fact::CollectionEntryAdded { .. }))
            .count();
        assert_eq!(added, 2, "and both entries still went in");
    }

    /// **But a play is counted, so the rule is a list and not a guess.**
    ///
    /// `RecordingPlayed` carries no timestamp: two plays of one track are the
    /// same fact told apart only by their stamps. Nothing proposes one today —
    /// a play is recorded by the player, not proposed — so this cannot be
    /// reached through `parse`, and asserting the predicate is the honest way
    /// to say it. Somebody adding a `Played` proposal has to come past this.
    #[test]
    fn a_counted_fact_is_not_a_declaration() {
        assert!(declares_a_concept(&Fact::CollectionNamed {
            collection: Id::<Collection>::of_name("Dance"),
            name: "Dance".into(),
            ordered: false,
        }));
        assert!(
            !declares_a_concept(&Fact::RecordingPlayed {
                recording: rec(1),
                encoding: None,
            }),
            "deduplicating this would count one play where there were two"
        );
        assert!(
            !declares_a_concept(&Fact::CollectionEntryAdded {
                collection: Id::<Collection>::of_name("Dance"),
                recording: rec(1),
                ord: crate::model::ord::OrdKey::first(),
            }),
            "an entry is membership, not a declaration"
        );
    }

    /// **Applying the same file twice changes nothing**, which is what makes a
    /// seeding pass safe to re-run against a live log.
    #[test]
    fn seeding_twice_is_seeding_once() {
        let file = format!("{}\n{}", line(1, "Dance"), line(2, "Dance"));
        let (first, _) = parse(&file, dev(), 100, HELD);
        let (again, _) = parse(&file, dev(), 200, HELD);

        // Compared as rendered facts, since a `Fact` is not `Ord` — and the
        // stamps deliberately differ, so it is the facts that must not.
        let facts = |rs: &BTreeSet<Record>| {
            rs.iter()
                .map(|r| format!("{:?}", r.fact))
                .collect::<BTreeSet<String>>()
        };
        assert_eq!(
            facts(&first),
            facts(&again),
            "a second pass proposed different facts"
        );
    }

    /// Two spellings are one collection, because the id is the normalised name
    /// — the same rule every other named concept follows.
    #[test]
    fn two_spellings_name_one_collection() {
        let (records, _) = parse(
            &format!("{}\n{}", line(1, "Dance"), line(2, "  dance  ")),
            dev(),
            100,
            HELD,
        );
        let ids: BTreeSet<_> = records
            .iter()
            .filter_map(|r| match &r.fact {
                Fact::CollectionEntryAdded { collection, .. } => Some(*collection),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 1, "one collection became two");
    }

    /// A nameless collection is refused rather than folded into one nobody can
    /// find, which is the same call `QualifierAssigned` makes.
    #[test]
    fn a_collection_with_no_name_is_refused() {
        let (_, report) = parse(&line(1, "   "), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    /// And a recording the library does not hold is named, not invented — the
    /// rule every proposal follows.
    #[test]
    fn a_recording_the_library_does_not_hold_is_refused() {
        let (_, report) = parse(&line(99, "Dance"), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.unknown.len(), 1, "{report}");
    }
}

#[cfg(test)]
mod credited {
    //! **Splitting a joined credit back into the artists it names** — #152.
    //!
    //! Without a MusicBrainz artist id an artist is minted from the credit
    //! *string*, so this library holds an artist called
    //! `Armand Van Helden,Dizzee Rascal` and four Stephan Bodzins. The model
    //! was always ready — `CreditAsserted` carries a position precisely so a
    //! multi-artist credit is several rows — and only the proposal was
    //! missing.
    //!
    //! Most of these tests are about **refusing**, because the failure is not
    //! symmetric: a split is repaired by a merge (#88) and a merge is repaired
    //! by nothing.

    use super::tests::*;
    use super::*;

    fn line(r: u128, artists: &str) -> String {
        format!(
            r#"{{"fact":"credited","recording":"{}","artists":{artists},"basis":"musicbrainz"}}"#,
            rec(r)
        )
    }

    #[test]
    fn a_joined_credit_becomes_one_row_per_artist_in_order() {
        let (records, report) = parse(
            &line(1, r#"["Armand van Helden","Dizzee Rascal"]"#),
            dev(),
            100,
            HELD,
        );
        assert_eq!(report.applied, 1, "{report}");

        let credits: Vec<(u8, Id<Artist>)> = records
            .iter()
            .filter_map(|r| match &r.fact {
                Fact::CreditAsserted {
                    position, artist, ..
                } => Some((*position, *artist)),
                _ => None,
            })
            .collect();
        assert_eq!(credits.len(), 2, "one row per artist: {credits:?}");
        assert_eq!(credits[0].0, 0);
        assert_eq!(
            credits[1].0, 1,
            "the order the credit names them is the position"
        );
        assert_eq!(
            credits[0].1,
            Id::<Artist>::of_artist(None, "Armand van Helden"),
            "the id is the one `of_artist` mints, so two devices agree"
        );
    }

    #[test]
    fn every_artist_is_named_before_it_is_credited() {
        let (records, _) = parse(&line(1, r#"["Kiko","Spencer K"]"#), dev(), 100, HELD);
        let named: Vec<&String> = records
            .iter()
            .filter_map(|r| match &r.fact {
                Fact::ArtistNamed { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(
            named,
            vec!["Kiko", "Spencer K"],
            "an id no fact resolves is unrenderable"
        );
    }

    /// **The basis is on the wire**, for the reason it is on a genre.
    ///
    /// The row this supersedes was a tag's reading. Folding the correction to
    /// the same basis would make it indistinguishable from the thing it
    /// corrects.
    #[test]
    fn the_correction_does_not_arrive_as_another_tag_reading() {
        let (records, _) = parse(&line(1, r#"["Kiko","Spencer K"]"#), dev(), 100, HELD);
        assert!(
            records.iter().all(|r| !matches!(
                &r.fact,
                Fact::CreditAsserted {
                    basis: Source::TagVariant,
                    ..
                }
            )),
            "a curated split must not read as a tag"
        );
    }

    /// **One artist is a rename wearing a repair's clothes**, and refused.
    #[test]
    fn a_split_into_one_artist_is_refused() {
        let (_, report) = parse(&line(1, r#"["Just The One"]"#), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    /// A duplicate name would fold two credits into one row and lose the
    /// second, so the count would silently disagree with the credit.
    #[test]
    fn the_same_artist_credited_twice_is_refused() {
        let (_, report) = parse(&line(1, r#"["Onno","onno"]"#), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    #[test]
    fn an_empty_name_is_refused() {
        let (_, report) = parse(&line(1, r#"["Onno","  "]"#), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }
}

#[cfg(test)]
mod merged {
    //! **Two rows that are one performance** — #88.
    //!
    //! A FLAC bought against an MP3 already held split eight tracks on this
    //! library: both sides carried a correct MusicBrainz id and the ids were
    //! different, because the two copies were tagged from different releases.
    //! A relink would have moved the binding and stranded what hung off the
    //! old row — 1 play, 8 collection memberships and 53 concept assertions.

    use super::tests::*;
    use super::*;

    fn line(loser: u128, winner: u128) -> String {
        format!(
            r#"{{"fact":"merged","loser":"{}","winner":"{}"}}"#,
            rec(loser),
            rec(winner)
        )
    }

    #[test]
    fn a_merge_says_which_row_survives() {
        let (records, report) = parse(&line(1, 2), dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");
        let facts: Vec<&Fact> = records.iter().map(|r| &r.fact).collect();
        assert!(
            facts.iter().any(|f| matches!(
                f,
                Fact::RecordingMerged { loser, winner } if *loser == rec(1) && *winner == rec(2)
            )),
            "the direction is carried, not inferred: {facts:?}"
        );
    }

    /// Folding this to a no-op would hide its usual cause — a pass that mapped
    /// both sides of a pair through one lookup and got one answer back.
    #[test]
    fn a_recording_cannot_absorb_itself() {
        let (_, report) = parse(&line(1, 1), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    /// The **winner** is the subject, because that is the row that has to
    /// exist. A loser nothing holds is an ordinary row.
    #[test]
    fn the_winner_is_what_the_library_is_asked_about() {
        let p = Proposal::Merged {
            loser: rec(9),
            winner: rec(2),
        };
        assert_eq!(p.subject(), Subject::Recording(rec(2)));
    }

    // ---- #155: a tracklist becomes slots ----

    fn tracklisted(release: uuid::Uuid, tracks: &[(u16, u128, &str)]) -> String {
        let listed = Proposal::Tracklisted {
            release,
            position: 1,
            format: Some("CD".into()),
            tracks: tracks
                .iter()
                .map(|(at, mbid, title)| Slotted {
                    position: *at,
                    recording: uuid::Uuid::from_u128(*mbid),
                    title: (*title).to_string(),
                    artists: vec!["Adele".into()],
                    title_on_release: None,
                })
                .collect(),
        };
        serde_json::to_string(&listed).expect("a proposal serialises")
    }

    /// **The slot exists and nothing fills it**, which is how this model has
    /// always said "this track is on the record and I do not have it" — and
    /// what nothing had ever written.
    #[test]
    fn a_tracklist_writes_a_slot_for_a_track_no_file_holds() {
        let text = tracklisted(pressed(), &[(1, 0x51, "Rolling in the Deep")]);
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert!(report.malformed.is_empty(), "{report}");
        assert_eq!(report.applied, 1, "{report}");

        let recording = Id::<Recording>::of_basis(&Basis::MusicBrainz(uuid::Uuid::from_u128(0x51)));
        let medium = Id::<Medium>::of_medium(Id::<Release>::of_musicbrainz(pressed()), 1);
        let facts: Vec<_> = records.iter().map(|r| r.fact.clone()).collect();

        assert!(
            facts.contains(&Fact::TrackPlaced {
                track: Id::<Track>::of_track(medium, recording),
                medium,
                recording,
                position: 1,
                title_on_release: None,
            }),
            "the slot itself"
        );
        assert!(
            facts.contains(&Fact::RecordingCatalogued {
                recording,
                title: "Rolling in the Deep".into(),
                artist: Id::<Artist>::of_artist(None, "Adele"),
            }),
            "a greyed row still needs a name to print"
        );
        assert!(
            !facts
                .iter()
                .any(|f| matches!(f, Fact::EncodingPlaced { .. })),
            "**nothing is placed**: a slot with no copy is the entire point, \
             and inventing an encoding for it would put a file nowhere"
        );
    }

    /// **The denominator #134 asked for**, filled from an authority for the
    /// first time. It is stated wholesale because the fold takes the row
    /// wholesale — which is why `format` is on the wire and not dropped.
    #[test]
    fn a_tracklist_says_how_long_the_record_is() {
        let text = tracklisted(pressed(), &[(1, 0x51, "One"), (2, 0x52, "Two")]);
        let (records, _) = parse(&text, dev(), 100, HELD);
        let medium = Id::<Medium>::of_medium(Id::<Release>::of_musicbrainz(pressed()), 1);
        assert!(
            records.iter().any(|r| r.fact
                == Fact::MediumCatalogued {
                    medium,
                    release: Id::<Release>::of_musicbrainz(pressed()),
                    position: 1,
                    format: Some("CD".into()),
                    tracks: Some(2),
                }),
            "two slots is a two-track disc, and the format survives the restatement"
        );
    }

    /// **The safety property.** A tracklist's ids agree with `import`'s only
    /// where the files carried the album's MusicBrainz id. Applied blind
    /// against a record the library has never seen, it would catalogue a
    /// phantom disc beside the real one — so it is stale, and says so.
    #[test]
    fn a_tracklist_for_a_record_the_library_has_never_seen_is_stale() {
        let text = tracklisted(uuid::Uuid::from_u128(0xdead), &[(1, 0x51, "One")]);
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 0, "{report}");
        assert_eq!(report.unknown.len(), 1, "{report}");
        assert!(records.is_empty(), "nothing is written for a stale line");
    }

    /// A slot is keyed by which recording fills it, so two rows naming one
    /// recording are one slot — and a list applied that way comes out shorter
    /// than the record it claims to describe. Refused, not quietly folded.
    #[test]
    fn a_tracklist_that_names_one_recording_twice_is_refused() {
        let text = tracklisted(pressed(), &[(1, 0x51, "One"), (2, 0x51, "One Again")]);
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    // ---- #162: one person, one role ----

    /// **The claim `Credited` cannot make.** That one is the split — several
    /// artists, all performers — and refuses a single name for good reason.
    #[test]
    fn one_person_can_be_credited_in_one_role() {
        let p = Proposal::Contributed {
            recording: rec(1),
            artist: "Andhim".into(),
            role: "remixer".into(),
            basis: Source::MusicBrainz,
        };
        let text = serde_json::to_string(&p).expect("serialises");
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let facts: Vec<_> = records.iter().map(|r| r.fact.clone()).collect();
        let role = Id::<Role>::of_name("remixer");
        assert!(
            facts.contains(&Fact::RoleDefined {
                role,
                name: "remixer".into(),
                class: RoleClass::Production,
            }),
            "**a remixer is Production, not Performer** — `line_of` reads \
             performers, so the wrong class puts the record in the remixer's \
             folder"
        );
        assert!(facts.contains(&Fact::CreditAsserted {
            subject: CreditOn::Recording(rec(1)),
            artist: Id::<Artist>::of_artist(None, "Andhim"),
            role,
            basis: Source::MusicBrainz,
            position: 0,
            as_credited: None,
            join_phrase: None,
        }));
    }

    /// **A role the codelist does not know is refused**, because the class
    /// travels with the name and a string does not say which class it is.
    #[test]
    fn a_role_nobody_defined_is_refused() {
        for role in ["remixer ", "REMIXER"] {
            let text = serde_json::to_string(&Proposal::Contributed {
                recording: rec(1),
                artist: "Andhim".into(),
                role: role.into(),
                basis: Source::MusicBrainz,
            })
            .expect("serialises");
            let (_, report) = parse(&text, dev(), 100, HELD);
            assert_eq!(report.applied, 1, "{role:?} is the same role: {report}");
        }
        for bad in ["remixxer", "vocalist", ""] {
            let text = serde_json::to_string(&Proposal::Contributed {
                recording: rec(1),
                artist: "Andhim".into(),
                role: bad.into(),
                basis: Source::MusicBrainz,
            })
            .expect("serialises");
            let (_, report) = parse(&text, dev(), 100, HELD);
            assert_eq!(report.refused.len(), 1, "{bad:?}: {report}");
        }
    }

    // ---- #162: one recording made out of another ----

    /// **The first thing to mint a `RecordingDerivation`.** The taxonomy has
    /// been built and dead for as long as it has existed.
    #[test]
    fn a_remix_is_derived_from_the_recording_it_was_made_out_of() {
        use crate::model::spine::RecordingDerivationKind;
        let original = uuid::Uuid::from_u128(0x0121);
        let p = Proposal::RecordingDerived {
            from: Basis::MusicBrainz(original),
            to: rec(2),
            kind: RecordingDerivationKind::Remix,
        };
        assert_eq!(
            p.subject(),
            Subject::Recording(rec(2)),
            "the derived recording is the row this is about"
        );
        let text = serde_json::to_string(&p).expect("serialises");
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let origin = Id::<Recording>::of_basis(&Basis::MusicBrainz(original));
        assert_eq!(
            records.iter().map(|r| r.fact.clone()).collect::<Vec<_>>(),
            vec![
                Fact::RecordingIdentified {
                    recording: origin,
                    basis: Basis::MusicBrainz(original),
                },
                Fact::RecordingDerived {
                    from: origin,
                    to: rec(2),
                    kind: RecordingDerivationKind::Remix,
                }
            ],
            "the far end is named, so an info panel can say what this was made out of"
        );
    }

    /// **The far end is a fact about the world, not a gap to fill** — an
    /// archivist, not a completionist (`library-api.md` §1).
    ///
    /// This refused when the library did not hold the original, on the
    /// argument that an edge into a row nothing holds says less than the
    /// qualifier it replaced. `Ref<T>` had said otherwise all along: *a
    /// reference is valid when its target is absent.* You own the remix; the
    /// original being elsewhere is what is true.
    #[test]
    fn a_derivation_names_an_original_the_library_does_not_hold() {
        use crate::model::spine::RecordingDerivationKind;
        let elsewhere = Basis::MusicBrainz(uuid::Uuid::from_u128(0xfeed));
        let text = serde_json::to_string(&Proposal::RecordingDerived {
            from: elsewhere.clone(),
            to: rec(2),
            kind: RecordingDerivationKind::Remix,
        })
        .expect("serialises");
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        // **And it stays out of the browse by construction.** `draw` builds
        // rows from `RecordingCatalogued`; nothing here writes one, so the
        // original is nameable and never drawn.
        assert!(
            !records
                .iter()
                .any(|r| matches!(r.fact, Fact::RecordingCatalogued { .. })),
            "a title would make this a row in a list of music you do not have"
        );
    }

    #[test]
    fn a_recording_cannot_be_derived_from_itself() {
        use crate::model::spine::RecordingDerivationKind;
        // The same performance by both names: the id minted from the basis is
        // the id the recording already has.
        let same = Basis::MusicBrainz(named());
        let text = serde_json::to_string(&Proposal::RecordingDerived {
            from: same.clone(),
            to: Id::<Recording>::of_basis(&same),
            kind: RecordingDerivationKind::Remix,
        })
        .expect("serialises");
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    // ---- #154: an artist that can be identified and never named ----

    /// The variant that was missing, and the two ends of one guard it serves.
    #[test]
    fn an_artist_can_be_given_a_name() {
        let p = Proposal::Named {
            artist: nameless(),
            name: "Tony Furtado".into(),
        };
        assert_eq!(p.subject(), Subject::Artist(nameless()));

        let text = serde_json::to_string(&p).expect("serialises");
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");
        assert_eq!(
            records.iter().map(|r| r.fact.clone()).collect::<Vec<_>>(),
            vec![Fact::ArtistNamed {
                artist: nameless(),
                name: "Tony Furtado".into(),
            }]
        );
    }

    /// **Held, not named.** The case this exists for is an artist with a right
    /// id and no label, so gating on having one would refuse the row it is
    /// meant to repair.
    #[test]
    fn an_artist_with_no_name_yet_is_still_a_subject_the_library_holds() {
        let p = Proposal::Named {
            artist: nameless(),
            name: "Tony Furtado".into(),
        };
        assert!(HELD.holds(p.subject()));
    }

    /// A label nobody can read is worse than none: `line_of` drops a credit
    /// whose artist has no name, and an empty one would print a separator
    /// introducing nobody.
    #[test]
    fn an_artist_cannot_be_named_nothing() {
        for name in ["", "   "] {
            let text = serde_json::to_string(&Proposal::Named {
                artist: nameless(),
                name: name.into(),
            })
            .expect("serialises");
            let (_, report) = parse(&text, dev(), 100, HELD);
            assert_eq!(report.refused.len(), 1, "{name:?}: {report}");
        }
    }

    // ---- #156: two album entries that are one album ----

    /// **The winner is the subject**, for the reason a recording merge's is:
    /// that is the row that has to exist. A loser nothing holds is ordinary.
    #[test]
    fn an_album_merge_is_asked_about_the_winner() {
        let p = Proposal::GroupsMerged {
            loser: Id::derived(b"somewhere else"),
            winner: album(),
        };
        assert_eq!(p.subject(), Subject::ReleaseGroup(album()));

        let text = serde_json::to_string(&p).expect("serialises");
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");
        assert_eq!(
            records.iter().map(|r| r.fact.clone()).collect::<Vec<_>>(),
            vec![Fact::ReleaseGroupMerged {
                loser: Id::derived(b"somewhere else"),
                winner: album(),
            }]
        );
    }

    /// A merge into an album nothing names is a redirect pointing nowhere.
    #[test]
    fn an_album_merge_into_one_the_library_lacks_is_stale() {
        let p = Proposal::GroupsMerged {
            loser: album(),
            winner: Id::derived(b"never heard of it"),
        };
        let text = serde_json::to_string(&p).expect("serialises");
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.unknown.len(), 1, "{report}");
    }

    #[test]
    fn an_album_cannot_absorb_itself() {
        let p = Proposal::GroupsMerged {
            loser: album(),
            winner: album(),
        };
        let text = serde_json::to_string(&p).expect("serialises");
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    /// **A retitle names a row, and refuses an empty one.**
    ///
    /// Unlike a missing artist name — which `line_of` can at least drop — a
    /// recording with no title cannot be found or played at all, so there is
    /// nothing to fall back to.
    #[test]
    fn a_retitle_needs_a_title_and_a_row_that_exists() {
        let good = Proposal::Retitled {
            recording: rec(1),
            title: "Give a Man a Rod".into(),
        };
        assert_eq!(good.subject(), Subject::Recording(rec(1)));
        let (records, report) = parse(
            &serde_json::to_string(&good).expect("serialises"),
            dev(),
            100,
            HELD,
        );
        assert_eq!(report.applied, 1, "{report}");
        assert!(matches!(
            records.iter().next().map(|r| &r.fact),
            Some(Fact::RecordingRetitled { title, .. }) if title == "Give a Man a Rod"
        ));

        let blank = Proposal::Retitled {
            recording: rec(1),
            title: "   ".into(),
        };
        let (_, refused) = parse(
            &serde_json::to_string(&blank).expect("serialises"),
            dev(),
            100,
            HELD,
        );
        assert_eq!(refused.refused.len(), 1, "{refused}");

        let stale = Proposal::Retitled {
            recording: Id::derived(b"never heard of it"),
            title: "Anything".into(),
        };
        let (_, unknown) = parse(
            &serde_json::to_string(&stale).expect("serialises"),
            dev(),
            100,
            HELD,
        );
        assert_eq!(unknown.unknown.len(), 1, "{unknown}");
    }

    /// **A credit can be withdrawn**, which nothing could do before.
    ///
    /// A joined credit mints one artist nobody is called and asserts it; #145
    /// stopped new ones being minted and left 356 already here. The case that
    /// forced it: merging Luna's two release groups united their credits, so
    /// the album was credited both to `Stephan Bodzin & Marc Romboy` — one
    /// artist, joined — and to the real one, and the folder was named after
    /// both.
    #[test]
    fn a_credit_can_be_withdrawn_and_names_a_role_the_codelist_knows() {
        let joined = Id::<Artist>::of_artist(None, "Stephan Bodzin & Marc Romboy");
        let good = Proposal::CreditWithdrawn {
            subject: CreditOn::ReleaseGroup(album()),
            artist: joined,
            role: "performer".into(),
            basis: Source::Manual,
        };
        assert_eq!(good.subject(), Subject::ReleaseGroup(album()));
        let (records, report) = parse(
            &serde_json::to_string(&good).expect("serialises"),
            dev(),
            100,
            HELD,
        );
        assert_eq!(report.applied, 1, "{report}");
        assert!(matches!(
            records.iter().next().map(|r| &r.fact),
            Some(Fact::CreditRetracted { artist, basis, .. })
                if *artist == joined && *basis == Source::Manual
        ));

        // **A role the codelist does not know shadows nothing** and would
        // report success — the quiet failure this refuses.
        let nonsense = Proposal::CreditWithdrawn {
            subject: CreditOn::ReleaseGroup(album()),
            artist: joined,
            role: "chief vibes officer".into(),
            basis: Source::Manual,
        };
        let (_, refused) = parse(
            &serde_json::to_string(&nonsense).expect("serialises"),
            dev(),
            100,
            HELD,
        );
        assert_eq!(refused.refused.len(), 1, "{refused}");
    }

    #[test]
    fn an_artist_cannot_absorb_themselves() {
        let p = Proposal::ArtistsMerged {
            loser: nameless(),
            winner: nameless(),
        };
        let text = serde_json::to_string(&p).expect("serialises");
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    /// **The split this repairs, end to end.**
    ///
    /// `of_artist` mints from the MusicBrainz id where the tags carry one and
    /// from the name otherwise, so one artist arriving both ways mints two —
    /// and the two ids here are minted the way the library really minted them
    /// rather than derived from literals, because that equality *is* the bug.
    #[test]
    fn an_artist_minted_both_ways_is_two_ids_and_one_merge_joins_them() {
        let mbid = uuid::Uuid::from_u128(0x0102);
        let named = Id::<Artist>::of_artist(None, "Umfang");
        let minted = Id::<Artist>::of_artist(Some(mbid), "Umfang");
        assert_ne!(named, minted, "the split this exists to repair");
        // And the casing that made three of the 82 visible folds away on its
        // own: it never was a second id, only a second label.
        assert_eq!(named, Id::<Artist>::of_artist(None, "UMFANG"));

        let p = Proposal::ArtistsMerged {
            loser: named,
            winner: minted,
        };
        assert_eq!(
            p.subject(),
            Subject::Artist(minted),
            "a merge is about the row that has to exist"
        );
    }

    /// **A tracklist and a credit repair agree**, rather than rendering the
    /// line twice — #152 and #155 asserting the same thing.
    ///
    /// The reason `artists` is a list. Sent as the joined line it would mint
    /// one artist called `A feat. B`, on the MusicBrainz rung, which outranks
    /// the tag it came from — so the pass that exists to split joined credits
    /// would be fighting a stronger claim of exactly the thing it repairs.
    #[test]
    fn a_tracklist_credits_the_same_rows_a_credit_repair_would() {
        let mbid = named();
        let recording = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        let who = ["Armand Van Helden", "Dizzee Rascal"];

        let listed = Proposal::Tracklisted {
            release: pressed(),
            position: 1,
            format: None,
            tracks: vec![Slotted {
                position: 1,
                recording: mbid,
                title: "Bonkers".into(),
                artists: who.iter().map(|s| (*s).to_string()).collect(),
                title_on_release: None,
            }],
        };
        let repair = Proposal::Credited {
            recording,
            artists: who.iter().map(|s| (*s).to_string()).collect(),
            basis: Source::MusicBrainz,
        };

        let credits = |p: &Proposal| -> Vec<Fact> {
            let text = serde_json::to_string(p).expect("serialises");
            let (records, report) = parse(&text, dev(), 100, HELD);
            assert_eq!(report.applied, 1, "{report}");
            records
                .iter()
                .map(|r| r.fact.clone())
                .filter(|f| matches!(f, Fact::CreditAsserted { .. } | Fact::ArtistNamed { .. }))
                .collect()
        };

        assert_eq!(
            credits(&listed),
            credits(&repair),
            "the same two artists, at the same positions, on the same rung"
        );
    }

    /// **The tool sends facts and the model mints**, as `Identified` does with
    /// a `Basis`. Nothing in Python reproduces `of_medium`'s seeding.
    #[test]
    fn the_medium_is_derived_from_the_release_and_the_disc_number() {
        let listed = Proposal::Tracklisted {
            release: pressed(),
            position: 2,
            format: None,
            tracks: vec![],
        };
        assert_eq!(
            listed.subject(),
            Subject::Medium(Id::<Medium>::of_medium(
                Id::<Release>::of_musicbrainz(pressed()),
                2
            ))
        );
    }
}
