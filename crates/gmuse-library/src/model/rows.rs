//! What a list needs to know about a row — issue #84.
//!
//! The seam's read contract, small enough to state in one trait.
//!
//! `ui/list.rs` takes a `&Scan` for one reason: to ask, of an arbitrary slice
//! of ids, what each row groups under and what it should say. That is a
//! question about the *library*, not about a store, and naming it as a trait is
//! what lets the answer come from somewhere else later without the renderer
//! noticing.
//!
//! Deliberately not `Row` from `seam.rs`. That one is the whole projection —
//! ratings, plays, quality, availability — assembled per recording. This is
//! what a **list** needs to draw a divider, which is three strings and a
//! number, borrowed rather than built. A view that scrolls past a thousand
//! rows should not allocate a thousand `String`s to decide where the albums
//! change.

use crate::model::entity::Facet;
use crate::model::id::Id;

/// What names a row: **the copy it draws**.
///
/// It was `store::CopyId`, and that name outlived what it named (#86). There
/// is no `store::Track` any more, and `spine::Track` — a position on a medium
/// — very much still exists, so `CopyId` would read as an id for the wrong
/// thing entirely. A row is about a *copy*, and this is its id.
///
/// **It is an `Id`, not a `Handle`** (#86). It was an index into the `Scan`'s
/// arena, which the librarian does not own and which will not exist — so a
/// row is named by the durable id of the bytes it draws.
///
/// The consequence is that two files with identical bytes are **one row**,
/// because they are one encoding. Measured on this library that is exactly
/// one pair, and that row is the truthful one: you hold one encoding, stored
/// at two paths. `HoldingsAsserted` is keyed by encoding, so the model could
/// not have said otherwise in any case.
///
/// What it buys is that a row id survives a restart, which is what lets a
/// session, a playlist and the undo stack name rows rather than paths.
/// What a row is, and the fixture helper beside it.
///
/// **Moved to `gmuse-ids` by #257.** The type is pure identity — two shapes over
/// `Id<Track>` and `Id<Recording>` — and `gmuse-engine` naming a row was pulling
/// the whole library crate in behind it. Re-exported here because a row is still
/// described in this module, and 67 call sites say this path.
pub use gmuse_ids::copy::CopyId;
#[cfg(any(test, feature = "testing"))]
pub use gmuse_ids::copy::row_of_test;

/// Where a row files, and what it is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listed<'a> {
    /// Who it files under — the album artist where there is one.
    pub file_under: &'a str,
    /// The album, or a stand-in when it has none.
    pub album: &'a str,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub title: &'a str,
    pub duration: std::time::Duration,

    // **Everything below is here because a display template asks for it** —
    // issue #78. `format::Template` read sixteen fields off a `Track`, which
    // is why five of the player's remaining reaches into the store were one
    // function. These are the fourteen a row can answer.
    //
    // The two it cannot are `path` and `file_name`. A row holds identities and
    // values and **never a location** — that omission is what stopped
    // `history.toml` being keyed by path, and it cost a four-star rating on a
    // file that still existed to learn. So a template may not ask for one, and
    // `Template::parse` refuses rather than rendering a blank.
    /// The track's own credit, which is not always who it files under.
    pub artist: Option<&'a str>,
    pub album_artist: Option<&'a str>,
    /// The title as tagged. `title` above is this or the file name — never
    /// empty — and a template can ask for either.
    pub title_if_named: Option<&'a str>,
    /// The album, **absent when nothing named one**.
    ///
    /// Distinct from `album` above only in that: `album` stands in a
    /// placeholder so a tree node always has a label. A template needs the
    /// difference — `[ — {album}]` is meant to disappear for a track on no
    /// album, and rendering the placeholder there would show the stand-in as
    /// though it were a name.
    ///
    /// **Was `tagged_album`, and the name was a lie** (#97). Both come from
    /// the fold; neither reads a file. Tags are a serialisation the library
    /// consumes at import and never consults again — but the name said
    /// otherwise, and a comment claiming a tag-versus-model distinction that
    /// does not exist got as far as the Decision Log before somebody asked.
    pub album_if_named: Option<&'a str>,
    /// Every concept asserted about this row, by the axis it sits on.
    ///
    /// **A set per axis, not a value** (#86, #117). The model holds
    /// `(subject, concept, source) -> confidence`, so many concepts per
    /// subject with their lineage is what it says; this field was one string
    /// because it was shaped after the store's single raw tag. Curation adds
    /// better-sourced assertions and retracts wrong ones — it never collapses
    /// them to one.
    ///
    /// Keyed by facet rather than one field per axis, which is what lets a new
    /// axis reach the query language without a row shape to widen.
    pub concepts: &'a std::collections::BTreeMap<Id<Facet>, Vec<String>>,
    /// **Who this row credits, as ids** — #179, normalised by #193.
    ///
    /// Not names. A name is a property of the *artist*, so storing one here
    /// would put it in two places — on every row that credits them, and in the
    /// artist record — and nothing would forbid the two disagreeing. Held as
    /// ids, the disagreeing state is not expressible, and `AliasAsserted`
    /// stops being a fact that has to reach every row.
    ///
    /// Measured before doing it: the strings a row stored weighed 0.82 MB
    /// across the library and 0.04 MB held once each — **21× duplication** —
    /// and assembling a credit line through the built index costs 0.78µs a
    /// row, so a whole-library draw pays about 6ms for it.
    ///
    /// What a *view* does with these is ask [`crate::query::filter::Named`],
    /// which answers with every name an artist goes by — **hidden ones
    /// included**, because a filter is exactly the caller `AliasKind::shown`
    /// does not apply to (#176). Search should reach `aphextwinsucks`; nothing
    /// should print it.
    pub credited: &'a [Id<crate::model::entity::Artist>],
    /// The imprint. Its own field because a label sampler's `album_artist` is
    /// "Various Artists" and the label has nowhere else to go.
    pub label: Option<&'a str>,
    /// Album, single, EP — the one answer (#133).
    pub release_type: crate::model::spine::ReleaseType,
    /// How much of the pressing this row sits on is here (#134).
    pub release_held: crate::model::seam::Held,
    /// And what else it also is. Empty almost always.
    pub release_secondary: &'a std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
    pub year: Option<i32>,
    pub bitrate: Option<u32>,
    /// **Where the spectrum stops, if anything has measured it.**
    ///
    /// The one technical field that is *not* read off the file's header — it
    /// is a `Fact::EncodingMeasured` somebody wrote, which is why it is
    /// `None` until a `quality` pass has run. That is the point of it: a
    /// bitrate is what a file *claims*, and this is what it turned out to be.
    ///
    /// Queryable so that "these want replacing" is a filter rather than a
    /// folder somebody moved things into — see CLAUDE.md, and the 56 albums
    /// that came back.
    pub cutoff_hz: Option<u32>,
    /// **How far it falls above the cutoff**, where anything measured it.
    /// Without this a cutoff cannot be read: an encoder's lowpass is a cliff
    /// and music is a slope, and `cutoff<16000` alone finds both.
    pub cliff_db: Option<u16>,
    /// **What the numbers say, as against what the file claims** — #254, #87.
    ///
    /// `suspect` when the declared bitrate is not supported by the audio, `unmeasured`
    /// when nothing has looked, `good` otherwise. Beside the two measurements because it
    /// is derived from them and the bitrate, and on the row because `Row.verdict` has
    /// said since #87 that this exists "so a re-buy list is a filter rather than a
    /// separate pass" — and until now nothing could filter on it.
    pub verdict: Option<crate::model::encoding::Verdict>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// **Whether the library holds a file for this row at all** — #155.
    ///
    /// `false` is a track the library knows is on the record and has no copy
    /// of anywhere, which is the row this issue exists to be able to draw. It
    /// is a property of the *recording*, not of the slot: a song you own off
    /// its own album is playable from a compilation's row too, and greying
    /// that one would be claiming you cannot hear it.
    ///
    /// Distinct from #38's Local/Remote/Absent, which asks *where* the bytes
    /// are. This asks whether there are any.
    pub held: bool,
}

/// A drawn row the library **owns**.
///
/// [`Listed`] borrows, which suits a store that holds `Track`s with the
/// strings already in them. The librarian does not: `file_under` is derived
/// from the album-level credit and `album` from the slot a copy sits in, so
/// there is nothing to lend a view of. It materialises these once per fold and
/// lends from that (#86).
///
/// It is also what crosses a thread. The remote hands a caller the current row
/// so it can render its own template, and a borrow cannot go with it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drawn {
    pub file_under: String,
    pub album: String,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub title: String,
    pub duration: std::time::Duration,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub title_if_named: Option<String>,
    pub album_if_named: Option<String>,
    pub concepts: std::collections::BTreeMap<Id<Facet>, Vec<String>>,
    /// See [`Listed::credited`] — who the row credits, as ids.
    pub credited: Vec<Id<crate::model::entity::Artist>>,
    pub label: Option<String>,
    pub release_type: crate::model::spine::ReleaseType,
    pub release_held: crate::model::seam::Held,
    pub release_secondary: std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
    pub year: Option<i32>,
    pub bitrate: Option<u32>,
    pub cutoff_hz: Option<u32>,
    /// **How far it falls above the cutoff**, where anything measured it.
    /// Without this a cutoff cannot be read: an encoder's lowpass is a cliff
    /// and music is a slope, and `cutoff<16000` alone finds both.
    pub cliff_db: Option<u16>,
    /// **What the numbers say, as against what the file claims** — #254, #87.
    ///
    /// `suspect` when the declared bitrate is not supported by the audio, `unmeasured`
    /// when nothing has looked, `good` otherwise. Beside the two measurements because it
    /// is derived from them and the bitrate, and on the row because `Row.verdict` has
    /// said since #87 that this exists "so a re-buy list is a filter rather than a
    /// separate pass" — and until now nothing could filter on it.
    pub verdict: Option<crate::model::encoding::Verdict>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub held: bool,
}

impl Listed<'_> {
    /// What kind of release this sits on, as words — #133.
    ///
    /// **The primary type and every secondary together**, because both answer
    /// the same question. Somebody asking for compilations means an
    /// `album/compilation` too, and a filter that only read the primary would
    /// answer "album" and find none of them.
    pub fn release_types(&self) -> Vec<String> {
        std::iter::once(self.release_type.written().to_string())
            .chain(
                self.release_secondary
                    .iter()
                    .map(|s| s.written().to_string()),
            )
            .collect()
    }
}

impl Listed<'_> {
    /// A copy that owns its strings.
    ///
    /// **What crosses a thread** (#86). The remote hands a caller the current
    /// row so it can render its own template, and a borrow cannot go with it.
    /// Taken from whichever side of the seam answered, so the caller does not
    /// have to know.
    pub fn owned(&self) -> Drawn {
        Drawn {
            file_under: self.file_under.to_string(),
            album: self.album.to_string(),
            disc: self.disc,
            track: self.track,
            title: self.title.to_string(),
            duration: self.duration,
            artist: self.artist.map(str::to_string),
            album_artist: self.album_artist.map(str::to_string),
            credited: self.credited.to_vec(),
            title_if_named: self.title_if_named.map(str::to_string),
            album_if_named: self.album_if_named.map(str::to_string),
            concepts: self.concepts.clone(),
            label: self.label.map(str::to_string),
            release_type: self.release_type,
            release_held: self.release_held,
            release_secondary: self.release_secondary.clone(),
            year: self.year,
            bitrate: self.bitrate,
            cutoff_hz: self.cutoff_hz,
            cliff_db: self.cliff_db,
            verdict: self.verdict,
            sample_rate: self.sample_rate,
            channels: self.channels,
            held: self.held,
        }
    }
}

impl Listed<'_> {
    /// The concepts on one axis, in the order the librarian settled on.
    ///
    /// Empty rather than absent when nothing has been asserted: "no genre" and
    /// "no such axis" are the same answer to a row, and the difference belongs
    /// to the parser, which refuses an axis nobody declared.
    pub fn on(&self, facet: Id<Facet>) -> &[String] {
        self.concepts.get(&facet).map_or(&[], Vec::as_slice)
    }

    #[allow(
        dead_code,
        reason = "the convenience for the axis almost every caller means; used by tests today, and every production reader goes through `on` with the axis named — which is the point of #117"
    )]
    /// The genres, which is the axis almost every caller means.
    pub fn genre(&self) -> &[String] {
        self.on(Id::of_key(crate::model::facet::GENRE))
    }
}

impl Drawn {
    /// The borrowed view a renderer takes.
    pub fn listed(&self) -> Listed<'_> {
        Listed {
            file_under: &self.file_under,
            album: &self.album,
            disc: self.disc,
            track: self.track,
            title: &self.title,
            duration: self.duration,
            artist: self.artist.as_deref(),
            album_artist: self.album_artist.as_deref(),
            credited: &self.credited,
            title_if_named: self.title_if_named.as_deref(),
            album_if_named: self.album_if_named.as_deref(),
            concepts: &self.concepts,
            label: self.label.as_deref(),
            release_type: self.release_type,
            release_held: self.release_held,
            release_secondary: &self.release_secondary,
            year: self.year,
            bitrate: self.bitrate,
            cutoff_hz: self.cutoff_hz,
            cliff_db: self.cliff_db,
            verdict: self.verdict,
            sample_rate: self.sample_rate,
            channels: self.channels,
            held: self.held,
        }
    }
}

/// What is needed to *open* a row, as against to draw it.
///
/// Produced at the moment of playing and never stored. That is the whole
/// discipline: a player that cannot hold a locator cannot key anything by one,
/// and keying by path is what stranded three history entries and a four-star
/// rating on a file that still existed.
#[derive(Debug, Clone, PartialEq)]
pub struct Playable {
    /// Where this device can find it. A path today; a device's kind will say
    /// how to read it when a holding may be remote (#38).
    /// **Owned** — #261. It is one `String` and the seam may be a socket, which has no fold
    /// for a `&str` to point into. Every caller took a copy or compared it anyway.
    pub locator: String,
    /// The ReplayGain tags, if any. Turned into a number by the caller,
    /// because how loud to play is a player question and what the tags say is
    /// a library one.
    pub gain: Option<Loudness>,
}

/// ReplayGain as the tags carry it: decibels and a linear peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// Answering, for any id, what a list needs to draw it.
///
/// Implemented by `Scan` today and by the librarian when it takes over (#86).
/// The renderer names this rather than either, so the swap is a change of
/// argument and nothing else.
/// Everything a filter or a sort is given about one row.
///
/// Three answers to three different questions, and they are separate because
/// they are asked of different things and kept for different lengths of time:
/// what to *show* is a row, where the file *is* is a holding, and what history
/// hangs off is an identity.
///
/// **The locator is passed in rather than held** (#86). A row carries no
/// location — that omission is what stopped `history.toml` being keyed by one
/// — but `path=` and `file_name=` are selectors a person can write, and
/// answering them needs the file's whereabouts for exactly as long as the
/// comparison takes. Same rule the `.m3u` export follows.
pub struct Subject<'a> {
    pub row: Listed<'a>,
    pub locator: Option<&'a str>,
    /// What history hangs off — **the library's binding, not the tags'.**
    ///
    /// The filter used to mint this from a file's own tags, which is the
    /// identity a relink changes: after 377 files were identified it would
    /// have filtered on the recording they used to be.
    pub recording: Option<crate::model::id::Id<crate::model::entity::Recording>>,
}

pub trait Listing {
    /// **What the library says about one row, written out** — #261.
    ///
    /// A [`Slip`], not a [`Listed`]: this trait is the *seam*, and a reader on the other end of
    /// a socket has no catalogue to borrow from. It lends a `Listed` back, so everything that
    /// reads a row — the template, the list styling, every `Field` — is unchanged.
    ///
    /// **`Librarian::describe` is the borrowed one**, and the hot paths use it by name.
    /// `query::found` reads a row per row of the whole library on every keystroke of a picker
    /// and `Shelf::grouped` reads one per track: both hold a concrete library and neither goes
    /// through here. The two exist because the two jobs are different, not because one is old.
    fn listed(&self, id: CopyId) -> Option<Slip>;

    /// Every row there is.
    ///
    /// "What is in the library" is a library question, and a filter or a
    /// catalog narrows a set somebody else supplies — which is what lets them
    /// stop knowing where rows come from (#86).
    fn rows(&self) -> Vec<CopyId>;

    // **`at` lived here and is gone** (#118, #120).
    //
    // It turned a locator into a row — the one question on this seam that
    // started from a path — and existed for exactly one caller: reading an
    // `.m3u` back, because an m3u names locations. A playlist is an ordered
    // collection of recordings now and nothing reads an m3u at all, so the
    // last path-shaped question the player could ask has no asker.
    //
    // **Nothing on this seam now takes a location as an argument.** That is
    // the rule `library-api.md` states and the thing #94 could only half
    // enforce while this was here.

    /// What a collection holds, in order.
    ///
    /// On the seam because a catalog may be defined by one (#94), and a
    /// catalog is resolved through `Listing` like everything else the player
    /// reads.
    fn entries_of(
        &self,
        collection: crate::model::id::Id<crate::model::entity::Collection>,
    ) -> Vec<crate::model::id::Id<crate::model::entity::Recording>>;

    /// Which recording this row's audio holds.
    fn recording_of(
        &self,
        id: CopyId,
    ) -> Option<crate::model::id::Id<crate::model::entity::Recording>>;

    /// What the library says about one thing — #125, `info-panel.md`.
    ///
    /// **On the seam because the panel reads through it**, like every other
    /// view (`layers.md`). What comes back is already shaped for drawing:
    /// sections of links, claims and facts, with the basis on every claim.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — #126 is the panel that asks it"
    )]
    /// Resolve a name to a thing — #129.
    fn find(&self, of: crate::model::about::Kind, name: &str) -> crate::model::about::Found;

    /// **The recordings a focus stands for** — the grain a crate edit is made at.
    ///
    /// `Librarian::covers` says what each kind covers. Here so an attached view can ask
    /// it too: the panel's "filed in" section fans an artist out to their recordings, and
    /// filing or unfiling from that page writes one fact per recording.
    fn covers(&self, focus: crate::model::focus::Focus)
    -> Vec<Id<crate::model::entity::Recording>>;

    /// What to call one thing, without building the page about it — #128.
    ///
    /// Its own question because a breadcrumb wants five names and no pages,
    /// and `about` is the expensive one.
    fn label_of(&self, focus: crate::model::focus::Focus) -> String;

    /// **The history is handed in, not found here** — #256.
    ///
    /// It used to be a `bool`, and the rows came from `Librarian.records` — a clone of
    /// the whole log carried by every fold so that this one surface could read it. The
    /// log's owner answers now (`Desk::history`), and an empty slice is a caller that
    /// did not ask.
    fn about(
        &self,
        focus: crate::model::focus::Focus,
        happened: &[crate::model::seam::Happened],
    ) -> crate::model::about::About;

    /// Does this facet have a hierarchy to climb? — #79.
    ///
    /// **So `levels genre@1` can refuse rather than look identical.** A depth
    /// on a facet with no narrower-of edges leaves every concept standing for
    /// itself, which draws exactly the same tree as no depth at all — a
    /// control that quietly does nothing, and the failure this project keeps
    /// naming. The caller asks first and says so.
    fn places(&self, facet: crate::model::id::Id<crate::model::entity::Facet>) -> bool;

    /// **The whole fold, if this reader is one** — #261.
    ///
    /// Grouping a shelf needs the library rather than a read of it: seven of `Librarian`'s own
    /// methods, none of which a client on a socket could answer and none of which it would ever
    /// ask, because it does not group anything — the engine did that before it replied.
    ///
    /// So rather than a second trait nobody else implements, or a `Context` field every call
    /// site has to carry, the question is asked of the reader: **can you give me all of it?**
    /// A fold says yes and a socket says no, and `ui::holding` picks an implementation on the
    /// answer. `None` is the default because it is the honest answer for everything that is not
    /// literally a fold.
    fn whole(&self) -> Option<&crate::model::seam::Librarian> {
        None
    }

    /// **The row for a recording**, as the library would draw it.
    ///
    /// Here rather than on `Librarian` alone (#261): it is a read about one thing, which is
    /// what this trait is for, and a view that asks it must not have to hold a fold to.
    fn row(
        &self,
        recording: Id<crate::model::entity::Recording>,
    ) -> Option<crate::model::seam::Row>;

    /// **Which file to play, and what it is** — the owned half of `Librarian::open`.
    ///
    /// `open` hands back `(&Encoding, &str)`, borrowed out of the fold, and a reader on the
    /// other end of a socket has no fold to borrow from. The one caller wanted three owned
    /// things out of it anyway: where the file is, which encoding it is, and how loud.
    fn chosen(
        &self,
        recording: Id<crate::model::entity::Recording>,
        here: &[Id<crate::model::entity::Device>],
        prefers: &[crate::model::encoding::Format],
    ) -> Option<(
        String,
        Id<crate::model::entity::Encoding>,
        crate::model::encoding::Levels,
    )>;

    /// Where to find it, and how loud the tags say it is.
    ///
    /// Separate from [`Listing::listed`] because they are asked at different
    /// moments and one of them must not be kept: a row is drawn now and
    /// forgotten, a locator is used immediately and never written down.
    fn playable(&self, id: CopyId) -> Option<Playable>;

    /// **Say which rows are about to be read, so a remote can fetch them in one trip** — #261.
    ///
    /// A no-op by default, and that is the honest answer for every implementation that
    /// already holds the rows: there is nothing to fetch and nothing to warm.
    ///
    /// It exists because a pane draws forty rows and the seam answers one at a time.
    /// Held locally that costs a map lookup each; held over a socket it was **forty round
    /// trips per frame asking for two thousand rows** — the read-ahead walks library order,
    /// which is the wrong order for a queue, so each of the forty missed on its own and
    /// pulled in fifty neighbours nobody would draw. Measured at 198 ms per keystroke in
    /// view 4 against 2.3 ms in the tree, which reads no rows at all.
    ///
    /// **A hint and not a read.** It returns nothing and promises nothing; a caller still
    /// asks [`Listing::listed`] for each row and must still cope with `None`. So an
    /// implementation is free to ignore it, and no correctness rests on it having run.
    fn ahead(&self, ids: &[CopyId]) {
        let _ = ids;
    }

    /// **Which copy this device holds of each recording named** — #261.
    ///
    /// The inverse of [`Listing::recording_of`], asked by everything that arrives holding
    /// recordings rather than copies: the engine's queue, a restored session, a collection.
    /// A recording outlives a file and a path, so it is what crosses the wire and what a
    /// session stores — and a pane draws copies, so the pairing is made again on every launch.
    ///
    /// **Batched by what is wanted, not by what there is.** The default walks every row, which
    /// is what the library is keyed by, and is why callers used to build the whole inverse map
    /// once and look up a hundred times. That is the right trade in this process and the wrong
    /// one over a socket, where the walk was one round trip per row: **706–956 ms** for 7,273
    /// rows to answer a queue of 107.
    ///
    /// A recording held as several encodings answers with one of them. Which to prefer is
    /// `shelf copies`' question and is not decidable here — this only says the device has one.
    fn copies_of(
        &self,
        recordings: &[Id<crate::model::entity::Recording>],
    ) -> std::collections::HashMap<Id<crate::model::entity::Recording>, CopyId> {
        let want: std::collections::HashSet<_> = recordings.iter().copied().collect();
        self.rows()
            .into_iter()
            .filter_map(|id| Some((self.recording_of(id)?, id)))
            .filter(|(recording, _)| want.contains(recording))
            .collect()
    }

    /// How many distinct albums and artists a slice covers.
    ///
    /// Here rather than at the call site because it is the same fold every
    /// caller writes, and because an implementation that already knows may
    /// answer without walking.
    fn spread(&self, ids: &[CopyId]) -> (usize, usize) {
        let mut albums: Vec<(String, String)> = Vec::new();
        let mut artists: Vec<String> = Vec::new();
        for id in ids {
            if let Some(l) = self.listed(*id) {
                let album = (l.file_under.clone(), l.album);
                if !albums.contains(&album) {
                    albums.push(album);
                }
                if !artists.contains(&l.file_under) {
                    artists.push(l.file_under);
                }
            }
        }
        (albums.len(), artists.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Owning a row must not quietly drop a field.**
    ///
    /// The remote takes this copy across a thread and renders a caller's own
    /// template from it, so anything left behind is a field that is simply
    /// blank over the socket and correct on screen — which is the kind of
    /// difference nobody reports as a bug. Sixteen fields copied by hand is
    /// sixteen chances to miss one, and a mutation dropping `genre` passed
    /// every other test in the suite.
    #[test]
    fn owning_a_row_keeps_every_field() {
        let full = Drawn {
            file_under: "Boards of Canada".into(),
            album: "Music Has the Right to Children".into(),
            disc: Some(1),
            track: Some(4),
            title: "Roygbiv".into(),
            duration: std::time::Duration::from_secs(151),
            artist: Some("Boards of Canada".into()),
            album_artist: Some("Boards of Canada".into()),
            credited: Vec::new(),
            title_if_named: Some("Roygbiv".into()),
            album_if_named: Some("Music Has the Right to Children".into()),
            concepts: std::collections::BTreeMap::from([(
                Id::of_key(crate::model::facet::GENRE),
                vec!["Electronic".into(), "IDM".into()],
            )]),
            label: Some("Warp".into()),
            // Not the defaults, so a round trip that dropped them would show.
            release_type: crate::model::spine::ReleaseType::Ep,
            release_held: crate::model::seam::Held::Partial,
            release_secondary: [crate::model::spine::ReleaseSecondary::Live]
                .into_iter()
                .collect(),
            year: Some(1998),
            bitrate: Some(320),
            cutoff_hz: Some(15_700),
            cliff_db: Some(5_200),
            verdict: None,
            sample_rate: Some(44_100),
            channels: Some(2),
            held: true,
        };
        assert_eq!(full.listed().owned(), full, "a field was lost on the way");
    }

    /// And the empty case, so "everything is `None`" cannot pass by accident.
    #[test]
    fn owning_an_empty_row_keeps_it_empty() {
        let bare = Drawn::default();
        assert_eq!(bare.listed().owned(), bare);
    }
}

/// **A row written out, for a reader that does not hold the fold** — #261.
///
/// [`Listed`] is a *view into the library*: every string in it borrows from the catalogue, which
/// is what makes a filter over 7,273 rows cost nothing per row. A client on the other end of a
/// socket has no catalogue to borrow from, so what crosses the wire has to own what it says.
///
/// **It lends a `Listed` back**, which is why nothing that reads one had to change. The format
/// template, the list styling and every `Field` still take `&Listed<'_>`; this hands them one
/// borrowing from itself. Two representations, one reader.
///
/// ## What it costs, measured
///
/// `just query-cost`, against the real library:
///
/// | | |
/// | --- | --- |
/// | a screenful, 40 rows | **25.8 µs** |
/// | every row, 7,273 | 3.0 ms |
///
/// A screenful is nothing. The whole library is 3.0 ms, which is a fifth of a frame — so this
/// is affordable because the tree pane draws a window now (`tree-over-the-wire.md` §7) and
/// would have been a poor trade when it drew 8,932 lines.
///
/// **`Listed` stays borrowed, and that is not a legacy.** `query::filter` reads one per row of
/// the whole library on every keystroke of a picker, where the 3.0 ms above would be the whole
/// cost of the query several times over. Two representations because the two jobs are
/// genuinely different: one is read once and drawn, the other is read 7,273 times and thrown
/// away.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Slip {
    pub file_under: String,
    pub album: String,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub title: String,
    pub duration_ms: u64,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub title_if_named: Option<String>,
    pub album_if_named: Option<String>,
    pub concepts: std::collections::BTreeMap<Id<Facet>, Vec<String>>,
    pub credited: Vec<Id<crate::model::entity::Artist>>,
    pub label: Option<String>,
    pub release_type: crate::model::spine::ReleaseType,
    pub release_held: crate::model::seam::Held,
    pub release_secondary: std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
    pub year: Option<i32>,
    pub bitrate: Option<u32>,
    pub cutoff_hz: Option<u32>,
    pub cliff_db: Option<u16>,
    pub verdict: Option<crate::model::encoding::Verdict>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub held: bool,
}

impl Slip {
    /// Write out what the library said about a row.
    pub fn of(listed: &Listed<'_>) -> Self {
        Slip {
            file_under: listed.file_under.to_owned(),
            album: listed.album.to_owned(),
            disc: listed.disc,
            track: listed.track,
            title: listed.title.to_owned(),
            // **Milliseconds, not a `Duration`.** A duration is two numbers with a unit
            // convention, and the wire has neither — every other length in the protocol is
            // `_ms` and this is not the place to be different.
            duration_ms: listed.duration.as_millis() as u64,
            artist: listed.artist.map(str::to_owned),
            album_artist: listed.album_artist.map(str::to_owned),
            title_if_named: listed.title_if_named.map(str::to_owned),
            album_if_named: listed.album_if_named.map(str::to_owned),
            concepts: listed.concepts.clone(),
            credited: listed.credited.to_vec(),
            label: listed.label.map(str::to_owned),
            release_type: listed.release_type,
            release_held: listed.release_held,
            release_secondary: listed.release_secondary.clone(),
            year: listed.year,
            bitrate: listed.bitrate,
            cutoff_hz: listed.cutoff_hz,
            cliff_db: listed.cliff_db,
            verdict: listed.verdict,
            sample_rate: listed.sample_rate,
            channels: listed.channels,
            held: listed.held,
        }
    }

    /// The same row, as every reader already expects it.
    pub fn listed(&self) -> Listed<'_> {
        Listed {
            file_under: &self.file_under,
            album: &self.album,
            disc: self.disc,
            track: self.track,
            title: &self.title,
            duration: std::time::Duration::from_millis(self.duration_ms),
            artist: self.artist.as_deref(),
            album_artist: self.album_artist.as_deref(),
            title_if_named: self.title_if_named.as_deref(),
            album_if_named: self.album_if_named.as_deref(),
            concepts: &self.concepts,
            credited: &self.credited,
            label: self.label.as_deref(),
            release_type: self.release_type,
            release_held: self.release_held,
            release_secondary: &self.release_secondary,
            year: self.year,
            bitrate: self.bitrate,
            cutoff_hz: self.cutoff_hz,
            cliff_db: self.cliff_db,
            verdict: self.verdict,
            sample_rate: self.sample_rate,
            channels: self.channels,
            held: self.held,
        }
    }
}

#[cfg(test)]
mod slip_is_the_same_row {
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::operations::scan::Holdings;
    use crate::query::format::{Field, Template};
    use std::path::PathBuf;
    use std::time::Duration;

    /// A library with the fields filled in, because a row of blanks compares equal to anything.
    fn library() -> crate::model::seam::Librarian {
        let mut held = Holdings::new();
        for (n, title) in ["The Sea", "Bells of Every Chapel", "In Dreams"]
            .iter()
            .enumerate()
        {
            held.upsert(Track {
                path: PathBuf::from(format!("/m/Sierra Ferrell/Long Time Coming/{n}.mp3")),
                artist: Some("Sierra Ferrell".into()),
                albumartist: Some("Sierra Ferrell".into()),
                album: Some("Long Time Coming".into()),
                title: Some((*title).into()),
                tracknumber: Some(n as u32 + 1),
                discnumber: Some(1),
                date: Some(2021),
                genres: vec!["folk".into(), "country".into()],
                // **Not a whole second.** `{duration}` renders m:ss, so a slip that
                // rounded milliseconds away would read identically through every
                // template — which is why the round trip is also asserted directly.
                duration: Duration::from_millis(221_450),
                bitrate: Some(320),
                ..Default::default()
            });
        }
        crate::fixture::from_holdings(&held)
    }

    /// **A reader cannot tell a written-out row from a borrowed one** — #261.
    ///
    /// The whole design rests on it: `Slip` exists so a client with no catalogue can hold a
    /// row, and it lends a [`Listed`] back so nothing which reads one had to change. If the two
    /// ever answer differently, every template and every column that runs against a remote row
    /// is quietly wrong — and wrong in the worst way, because it renders.
    ///
    /// Asked of **every field there is**, by name, rather than the handful a test would think
    /// to list. A field added to `Listed` and forgotten in `Slip` is exactly the failure this
    /// guards, and a hand-written list would not see it.
    #[test]
    fn every_field_reads_the_same_through_a_slip() {
        let library = library();
        let rows = library.rows();
        assert!(
            rows.len() >= 3,
            "only {} rows in the fixture — this compares almost nothing",
            rows.len()
        );

        let names = Field::names();
        assert!(
            names.len() > 15,
            "only {} fields known, so 'every field' is not what is being asked",
            names.len()
        );
        // Two fields a row must never carry are refused by `Template::parse` outright, so the
        // ones that build a template are the ones a reader can actually ask for.
        let asks: Vec<(String, Template)> = names
            .iter()
            .filter_map(|n| {
                Template::parse(&format!("{{{n}}}"))
                    .ok()
                    .map(|t| ((*n).to_owned(), t))
            })
            .collect();
        assert!(
            asks.len() > 12,
            "only {} of {} fields could be asked for",
            asks.len(),
            names.len()
        );

        let mut answered = 0;
        for id in rows {
            let Some(listed) = library.describe(id) else {
                continue;
            };
            let slip = Slip::of(&listed);
            let lent = slip.listed();
            for (name, template) in &asks {
                let borrowed = template.render(&listed);
                let written = template.render(&lent);
                answered += usize::from(!borrowed.is_empty());
                assert_eq!(borrowed, written, "{name} reads differently through a slip");
            }
        }
        // **Both rendering empty everywhere would pass the loop above** — the classic vacuous
        // absence. A filled-in fixture answers a good many of its fields.
        assert!(
            answered > 20,
            "only {answered} field renders were anything at all, so equality proves nothing"
        );

        // **And the fields a template rounds off, compared directly.** `{duration}` renders
        // m:ss, so a slip that dropped the milliseconds would pass every render above; the
        // wire carries `duration_ms` precisely so it does not have to.
        let id = library.rows().into_iter().next().expect("a row");
        let listed = library.describe(id).expect("listed");
        let slip = Slip::of(&listed);
        assert_ne!(
            listed.duration.subsec_millis(),
            0,
            "the fixture's durations are whole seconds, so rounding is invisible"
        );
        assert_eq!(
            slip.listed().duration,
            listed.duration,
            "a slip rounded the duration"
        );
    }

    /// And it survives the wire, which is the only reason it owns anything.
    #[test]
    fn a_slip_survives_being_written_down() {
        let library = library();
        let id = library.rows().into_iter().next().expect("a row");
        let listed = library.describe(id).expect("the library lists it");
        let slip = Slip::of(&listed);

        let json = serde_json::to_string(&slip).expect("a slip serialises");
        assert!(
            json.len() > 100,
            "a slip of {} bytes is not a row",
            json.len()
        );
        let back: Slip = serde_json::from_str(&json).expect("and comes back");
        assert_eq!(back, slip, "a slip did not survive the wire");
    }
}
