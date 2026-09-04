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
use crate::model::entity::{Artist, Encoding, Release, ReleaseGroup, Work};
use crate::model::id::Id;

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

/// Derive an id from a MusicBrainz id, for the kinds that carry one.
macro_rules! from_musicbrainz {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Id<$ty> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(scheme: u8, hash: u128) -> AudioId {
        AudioId { scheme, hash }
    }

    /// The no-coordinator bet, at the level it is actually made: two machines
    /// hash the same file and agree on the entity without a conversation.
    #[test]
    fn the_same_file_mints_the_same_encoding_anywhere() {
        let a = audio(1, 0x8fb0_dcfb_f145_dfff_6649_7842_65da_0229);
        assert_eq!(Id::of_audio(&a), Id::of_audio(&a));
        assert_ne!(Id::of_audio(&a), Id::of_audio(&audio(1, 0x1234)));
    }

    /// A rescheme is a different measurement and mints a different id. That is
    /// intended: the point of deriving is that a redirect can reconcile them,
    /// where a hash used as the key could not have been repaired.
    #[test]
    fn a_scheme_bump_mints_a_different_encoding() {
        let hash = 0x8fb0_dcfb_f145_dfff_6649_7842_65da_0229;
        assert_ne!(Id::of_audio(&audio(1, hash)), Id::of_audio(&audio(2, hash)));
    }

    /// Derived, not adopted: the upstream id is a binding, so an upstream
    /// merge cannot rename anything of ours. Shown on a release, because a
    /// recording's id goes through `Basis` and is tested there (#89).
    #[test]
    fn an_entity_id_is_not_its_musicbrainz_id() {
        let mbid = Uuid::parse_str("183105b5-0000-4000-8000-000000000000").unwrap();
        let id = Id::<Release>::of_musicbrainz(mbid);
        assert_eq!(id, Id::<Release>::of_musicbrainz(mbid), "still convergent");
        assert_ne!(id.as_uuid(), mbid, "ours, derived from theirs");
    }

    /// One MusicBrainz id names a recording *and* nothing else. Kinds are
    /// separated in the seed, so a release and a recording sharing an id by
    /// accident stay separate entities.
    #[test]
    fn one_mbid_under_two_kinds_stays_two_entities() {
        let mbid = Uuid::parse_str("859d0860-0000-4000-8000-000000000000").unwrap();
        assert_ne!(
            Id::<Work>::of_musicbrainz(mbid).as_uuid(),
            Id::<Release>::of_musicbrainz(mbid).as_uuid()
        );
    }

    /// The literals every derived id in every log depends on.
    ///
    /// This changed exactly once, when `Distribution` became `Encoding` and
    /// took its `KIND` with it. That was free only because no log had yet been
    /// written: nothing on any disk held a derived id, so there was nothing to
    /// re-identify. It will never be free again, and this test is what makes
    /// the next attempt loud.
    #[test]
    fn the_minting_is_pinned() {
        let a = audio(1, 0x8fb0_dcfb_f145_dfff_6649_7842_65da_0229);
        assert_eq!(
            Id::of_audio(&a).to_string(),
            "fcda6141-233b-5b1f-8d1d-ec0af7926402",
            "encoding minting changed: every stored id is now wrong"
        );
        let mbid = Uuid::parse_str("183105b5-0000-4000-8000-000000000000").unwrap();
        assert_eq!(
            Id::<Release>::of_musicbrainz(mbid).to_string(),
            "1f4e63fd-2605-557d-ba5b-05ae0c641352",
            "release minting changed: every stored id is now wrong"
        );
    }
}
