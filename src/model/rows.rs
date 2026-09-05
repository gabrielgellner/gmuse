//! What a list needs to know about a row — issue #84.
//!
//! The seam's read contract, small enough to state in one trait.
//!
//! `ui/list.rs` takes a `&Store` for one reason: to ask, of an arbitrary slice
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

use crate::model::entity::Recording;
use crate::model::id::Handle;

/// The same thing `store::TrackId` aliases, named from this side.
///
/// Written out rather than imported from `store`, because the model must not
/// reach into the player (#77) — and because a handle is a model concept that
/// the store happens to hand out, not the other way round.
type Row = Handle<Recording>;

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
    pub genre: Option<&'a str>,
    /// The imprint. Its own field because a label sampler's `album_artist` is
    /// "Various Artists" and the label has nowhere else to go.
    pub label: Option<&'a str>,
    pub year: Option<i32>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
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
/// Implemented by `Store` today and by the librarian when it takes over (#86).
/// The renderer names this rather than either, so the swap is a change of
/// argument and nothing else.
pub trait Listing {
    fn listed(&self, id: Row) -> Option<Listed<'_>>;

    /// Where to find it, and how loud the tags say it is.
    ///
    /// Separate from [`Listing::listed`] because they are asked at different
    /// moments and one of them must not be kept: a row is drawn now and
    /// forgotten, a locator is used immediately and never written down.
    fn playable(&self, id: Row) -> Option<Playable<'_>>;

    /// What this row's *audio* is, so a caller can ask the library which
    /// recording holds it.
    ///
    /// A handle indexes an arena the library does not own, and the binding
    /// from bytes to performance lives in the library — so answering "which
    /// recording is this handle" takes one hop through each, and this is the
    /// half the handle's owner can give.
    fn audio_of(&self, id: Row) -> Option<crate::audio_id::AudioId>;

    /// How many distinct albums and artists a slice covers.
    ///
    /// Here rather than at the call site because it is the same fold every
    /// caller writes, and because an implementation that already knows may
    /// answer without walking.
    fn spread(&self, ids: &[Row]) -> (usize, usize) {
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
