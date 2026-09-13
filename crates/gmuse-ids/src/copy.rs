//! **What a playable row is** — #155, moved here by #257.
//!
//! This is an identity type and nothing more: two shapes built from `Id<Track>`
//! and `Id<Recording>`, with no fold, no projection and no log behind them. It
//! lived in `gmuse-library`'s `model::rows` because that is where rows are
//! described, and the description is a library concern — but the *identity* is
//! not, and `gmuse-engine` naming a row was reason enough to pull the whole
//! library crate in behind it.
//!
//! `model::rows` re-exports it, so nothing that already says
//! `gmuse_library::model::rows::CopyId` has to move.

use crate::entity::{Recording, Track};
use crate::id::Id;

/// What a row is — #155.
///
/// It was `Id<Encoding>`, so a row was a file. That drew an album held as FLAC
/// and MP3 twice, and could not draw a track at all until a file for it
/// existed.
///
/// **Two shapes, because both exist.** The Music Ontology gives a playable
/// item two independent relations — it `encodes` a signal, and a manifestation
/// is `available_as` it — and this library has the same pair in
/// `EncodingBound` and `EncodingPlaced`. So a recording may appear on releases
/// or on none, and MusicBrainz calls the second a *standalone recording*: an
/// internet-only track, a broadcast, a YouTube rip. Minting a release group to
/// give one a home is what `Various Artists/YouTube Sessions` already is, and
/// #20 is the issue for not doing that again.
///
/// **A row draws from three levels and is none of them.** Which album, which
/// position and whether anything fills it are the slot's; what plays is the
/// encoding's; the rating and the play count are the recording's, because
/// playing either copy of one performance is the same listening.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum CopyId {
    /// An appearance on a medium, which is the ordinary case.
    On(Id<Track>),
    /// A recording no release holds.
    Loose(Id<Recording>),
}

impl CopyId {
    /// A number to seed a shuffle with, stable across runs.
    ///
    /// **Durable, per #86.** Seeded from an arena index the order changed
    /// whenever a rescan renumbered it; both shapes here derive from a uuid,
    /// so a seed shuffles a library the same way twice.
    pub fn scalar(self) -> u128 {
        match self {
            CopyId::On(track) => track.scalar(),
            CopyId::Loose(recording) => recording.scalar(),
        }
    }

    /// The slot, where this row is an appearance.
    pub fn slot(self) -> Option<Id<Track>> {
        match self {
            CopyId::On(track) => Some(track),
            CopyId::Loose(_) => None,
        }
    }
}

/// A row id for a fixture, deterministic and distinct per number.
///
/// Tests used to write `TrackId::new(3)` — an arena index, which is exactly
/// what a row id stopped being (#86). Derived from the number so the same
/// fixture gets the same id every run, and so a test that asserts an ordering
/// asserts a real one.
#[cfg(any(test, feature = "testing"))]
pub fn row_of_test(n: u32) -> CopyId {
    CopyId::On(Id::derived(&n.to_le_bytes()))
}
