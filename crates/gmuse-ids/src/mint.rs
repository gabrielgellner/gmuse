//! Convergent minting — issue #66, `domain-model.md` §2 and §11.
//!
//! Two machines that have never met must agree on what an entity *is*. Where a
//! fact about the thing can serve as a seed, they can: the id is derived from
//! that fact rather than drawn at random, so both arrive at the same UUID
//! without coordinating. `library-sync-design.md` §2 requires exactly this of
//! anything that syncs — "identical on every device, generated without
//! coordination, and stable under retagging".
//!
//! ## Derive; do not adopt
//!
//! It is tempting to use a MusicBrainz recording id *as* `Id<Recording>`, since
//! it is already a UUID. §3 forbids it, and for a reason this library has
//! already been bitten by: MusicBrainz merged `183105b5…` into `859d0860…`, so
//! an adopted key would have had someone else's editorial decision change an
//! entry's identity. Derived, the upstream id is a **binding** — a fact we
//! record about the entity, not the entity's name for itself.
//!
//! ## What convergence does and does not promise
//!
//! Deriving from an external id is convergent only as far as that authority is
//! stable. Two devices holding different vintages of the same album — one
//! tagged before an upstream merge, one after — seed from different ids and
//! mint different recordings.
//!
//! That is not a hole, because it lands in the case §11 already handles: both
//! devices mint the *same* `Encoding` from the same bytes and each points
//! it at its own recording, which is one structural field taking two writes
//! from the same base. §4 routes that to a surfaced correction. **The
//! collision is the duplicate detector**, and convergent minting is the thing
//! that keeps it rare rather than the thing that has to be perfect.

use uuid::Uuid;

use crate::audio_id::AudioId;
use crate::entity::{Artist, Encoding, Medium, Release, ReleaseGroup, Track, Work};
use crate::id::Id;
use crate::spine::Recording;

impl Id<Encoding> {
    /// The id these exact bytes always mint.
    ///
    /// An encoding is the one entity whose identity is fully determined by
    /// something measurable, so this never falls back to a random mint.
    ///
    /// The seed is the whole `v1:…` form, **scheme included**. A future scheme
    /// therefore mints different ids — deliberately, because a differently
    /// framed hash is a different measurement. What makes that survivable is
    /// that these are ordinary `Id<T>`s: the redirect map can reconcile a v1
    /// and a v2 encoding, where an `AudioId` used *as* the key could not
    /// have been repaired at all.
    pub fn of_audio(audio: &AudioId) -> Self {
        Id::derived(audio.to_string().as_bytes())
    }
}

impl Id<crate::entity::View> {
    /// The id a saved view's name always mints.
    ///
    /// Convergent through [`crate::normalise`], like every
    /// other named thing here, so two devices saving `mornings` independently
    /// write one definition rather than two.
    ///
    /// The consequence is the usual one, and here it is a *feature*: renaming
    /// a view mints a new id, so the old name keeps resolving to the old
    /// definition until somebody drops it. A view referenced by name from
    /// another view therefore breaks loudly at use rather than silently
    /// resolving to something that has moved.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::normalise(name).as_bytes())
    }
}

impl Id<crate::entity::Rule> {
    /// The id a saved rule's name always mints — #323.
    ///
    /// Convergent for the reason a view's is: two devices saving `best`
    /// write one rule, and the later definition wins.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::normalise(name).as_bytes())
    }
}

impl Id<crate::entity::Collection> {
    /// The id a collection's name always mints.
    ///
    /// Through [`crate::normalise`], like every other named
    /// concept — so two devices creating "Dance" independently union into one
    /// collection rather than two, with no registry between them.
    ///
    /// The same trade it makes everywhere else: renaming mints a new id, which
    /// is a repairable split rather than an unrepairable merge.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::normalise(name).as_bytes())
    }
}

/// Derive an id from a MusicBrainz id, for the kinds that carry one.
macro_rules! from_musicbrainz {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Id<$ty> {
                #[allow(dead_code, reason = "expanded for several kinds and dead for only some of them, so `expect` would report itself unfulfilled on the ones that do have a caller — the one site where the self-retiring form does not work")]
                /// Derive this entity's id from its MusicBrainz id.
                ///
                /// Derived rather than adopted, so an upstream merge cannot
                /// change our identity — see the module docs.
                pub fn of_musicbrainz(mbid: Uuid) -> Self {
                    Id::derived(mbid.as_bytes())
                }
            }
        )*
    };
}

// **`Recording` is deliberately absent.** Its id comes from `Basis`, which is
// the whole of #89: a recording can be named by MusicBrainz, by some other
// authority, or by nothing but its bytes, and which of those it was has to be
// recorded rather than inferred. Leaving `of_musicbrainz` reachable here would
// leave a second door into the same identity — and two ladders that agree only
// until someone edits one is exactly the bug this replaced.
from_musicbrainz!(Work, Release, ReleaseGroup, Artist);

impl Id<Release> {
    /// A pressing, by MusicBrainz id where the tags carry one.
    ///
    /// **The fallback is the group itself**, not a second invented pressing.
    /// A scan sees one release per album — the one on disk — and the honest
    /// thing to say about an album whose tags name no release is "this is the
    /// pressing we hold", not "here is a release we know nothing about".
    /// Deriving from the group makes two devices holding the same untagged
    /// album agree, and the day either one learns the MusicBrainz id it mints
    /// the real one and the two reconcile by merge.
    pub fn of_release(mbid: Option<Uuid>, group: Id<ReleaseGroup>) -> Self {
        match mbid {
            Some(id) => Id::<Release>::of_musicbrainz(id),
            None => {
                let mut seed = b"group\x1f".to_vec();
                seed.extend_from_slice(group.as_uuid().as_bytes());
                Id::derived(&seed)
            }
        }
    }
}

impl Id<Medium> {
    /// A disc or a side, identified by **where it sits in its release**.
    ///
    /// There is nothing else to identify it by: media carry no ids of their
    /// own anywhere, and "disc 2 of this release" is what every tag, sleeve
    /// and catalogue actually says.
    pub fn of_medium(release: Id<Release>, position: u16) -> Self {
        let mut seed = release.as_uuid().as_bytes().to_vec();
        seed.push(0x1f);
        seed.extend_from_slice(&position.to_be_bytes());
        Id::derived(&seed)
    }
}

impl Id<Track> {
    /// A slot on a medium, identified by **which recording fills it** rather
    /// than by its number.
    ///
    /// The choice is between two errors and one of them is repairable. Keyed
    /// by recording, two devices that disagree about a track number still
    /// agree the slot is the same slot, and the number is an attribute a
    /// correction can move. Keyed by position, correcting a tracklist mints a
    /// new track for every row that shifted and strands whatever hung off the
    /// old ones — a false split with nothing to merge it back.
    ///
    /// What it costs is the album that plays one recording twice. That folds
    /// to one slot, and it is rare enough to be worth a merge later rather
    /// than making every renumbering destructive now.
    pub fn of_track(medium: Id<Medium>, recording: Id<Recording>) -> Self {
        let mut seed = medium.as_uuid().as_bytes().to_vec();
        seed.push(0x1f);
        seed.extend_from_slice(recording.as_uuid().as_bytes());
        Id::derived(&seed)
    }
}

// **The tests are in `gmuse::model::minting`, not here** — #213.
//
// They pin the derivation against strings that must never change, and the
// seeds they pin are assembled from both sides of this crate boundary: the
// facet keys live in `gmuse::model::facet` because a facet is a vocabulary,
// while the derivation lives here. A test needs every input at once, and only
// the library has them all.
//
// Splitting the table so each half sat with its own inputs was the
// alternative, and it makes the pin weaker: the value of that test is that one
// table shows every id this project has promised not to change.
