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

use crate::model::entity::{Encoding, Facet};
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
pub type CopyId = Id<Encoding>;

/// A row id for a fixture, deterministic and distinct per number.
///
/// Tests used to write `TrackId::new(3)` — an arena index, which is exactly
/// what a row id stopped being (#86). Derived from the number so the same
/// fixture gets the same id every run, and so a test that asserts an ordering
/// asserts a real one.
#[cfg(test)]
pub fn row_of_test(n: u32) -> CopyId {
    CopyId::derived(&n.to_le_bytes())
}

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
    pub tagged_title: Option<&'a str>,
    /// The album as tagged, **absent when there is none**.
    ///
    /// Distinct from `album` above, which stands in a placeholder so a tree
    /// node always has a label. A template needs the difference: `[ — {album}]`
    /// is meant to disappear for a track that is on no album, and rendering
    /// the placeholder there would show the stand-in as though it were a name.
    pub tagged_album: Option<&'a str>,
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
    /// The imprint. Its own field because a label sampler's `album_artist` is
    /// "Various Artists" and the label has nowhere else to go.
    pub label: Option<&'a str>,
    pub year: Option<i32>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
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
    pub tagged_title: Option<String>,
    pub tagged_album: Option<String>,
    pub concepts: std::collections::BTreeMap<Id<Facet>, Vec<String>>,
    pub label: Option<String>,
    pub year: Option<i32>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
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
            tagged_title: self.tagged_title.map(str::to_string),
            tagged_album: self.tagged_album.map(str::to_string),
            concepts: self.concepts.clone(),
            label: self.label.map(str::to_string),
            year: self.year,
            bitrate: self.bitrate,
            sample_rate: self.sample_rate,
            channels: self.channels,
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
            tagged_title: self.tagged_title.as_deref(),
            tagged_album: self.tagged_album.as_deref(),
            concepts: &self.concepts,
            label: self.label.as_deref(),
            year: self.year,
            bitrate: self.bitrate,
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }
}

/// What is needed to *open* a row, as against to draw it.
///
/// Produced at the moment of playing and never stored. That is the whole
/// discipline: a player that cannot hold a locator cannot key anything by one,
/// and keying by path is what stranded three history entries and a four-star
/// rating on a file that still existed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playable<'a> {
    /// Where this device can find it. A path today; a device's kind will say
    /// how to read it when a holding may be remote (#38).
    pub locator: &'a str,
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
    fn listed(&self, id: CopyId) -> Option<Listed<'_>>;

    /// Every row there is.
    ///
    /// "What is in the library" is a library question, and a filter or a
    /// catalog narrows a set somebody else supplies — which is what lets them
    /// stop knowing where rows come from (#86).
    fn rows(&self) -> Vec<CopyId>;

    /// The row a locator names, if this device holds one there.
    ///
    /// The inverse of [`Listing::playable`], and the one question that has to
    /// start from a path: an `.m3u` is a path format, so reading one back
    /// means turning a location into a row. **Asked at the moment of loading
    /// and not kept** — what the playlist holds afterwards is row ids.
    ///
    /// `None` for a path this device does not hold. A playlist can name music
    /// that has gone, or predate the scan that would have found it, and saying
    /// so is what stops it quietly coming up short.
    fn at(&self, locator: &str) -> Option<CopyId>;

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

    /// The three answers together, for a filter or a sort.
    fn subject(&self, id: CopyId) -> Option<Subject<'_>> {
        Some(Subject {
            row: self.listed(id)?,
            locator: self.playable(id).map(|p| p.locator),
            recording: self.recording_of(id),
        })
    }

    /// Where to find it, and how loud the tags say it is.
    ///
    /// Separate from [`Listing::listed`] because they are asked at different
    /// moments and one of them must not be kept: a row is drawn now and
    /// forgotten, a locator is used immediately and never written down.
    fn playable(&self, id: CopyId) -> Option<Playable<'_>>;

    /// How many distinct albums and artists a slice covers.
    ///
    /// Here rather than at the call site because it is the same fold every
    /// caller writes, and because an implementation that already knows may
    /// answer without walking.
    fn spread(&self, ids: &[CopyId]) -> (usize, usize) {
        let mut albums: Vec<(&str, &str)> = Vec::new();
        let mut artists: Vec<&str> = Vec::new();
        for id in ids {
            if let Some(l) = self.listed(*id) {
                let album = (l.file_under, l.album);
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
            tagged_title: Some("Roygbiv".into()),
            tagged_album: Some("Music Has the Right to Children".into()),
            concepts: std::collections::BTreeMap::from([(
                Id::of_key(crate::model::facet::GENRE),
                vec!["Electronic".into(), "IDM".into()],
            )]),
            label: Some("Warp".into()),
            year: Some(1998),
            bitrate: Some(320),
            sample_rate: Some(44_100),
            channels: Some(2),
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
