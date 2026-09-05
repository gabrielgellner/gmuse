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
use crate::model::entity::{Artist, Encoding, Medium, Release, ReleaseGroup, Track, Work};
use crate::model::id::Id;
use crate::model::spine::Recording;

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

impl Id<crate::model::entity::Collection> {
    /// The id a collection's name always mints.
    ///
    /// Through [`crate::model::ingest::normalise`], like every other named
    /// concept — so two devices creating "Dance" independently union into one
    /// collection rather than two, with no registry between them.
    ///
    /// The same trade it makes everywhere else: renaming mints a new id, which
    /// is a repairable split rather than an unrepairable merge.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::model::ingest::normalise(name).as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Concept;
    use crate::model::facet;

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
        use crate::model::basis::Basis;
        use crate::model::entity::{Artist, Label, Medium, Role, Track};
        use crate::model::spine::Recording;

        let a = audio(1, 0x8fb0_dcfb_f145_dfff_6649_7842_65da_0229);
        let mbid = Uuid::parse_str("183105b5-0000-4000-8000-000000000000").unwrap();
        let group = Id::<ReleaseGroup>::of_musicbrainz(mbid);
        let release = Id::<Release>::of_release(Some(mbid), group);
        let medium = Id::<Medium>::of_medium(release, 1);
        let recording = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));

        // Every derivation, with the input beside the answer, so a failure
        // says which rung moved rather than only that something did.
        let pinned: &[(&str, String, &str)] = &[
            (
                "encoding, from its bytes",
                Id::of_audio(&a).to_string(),
                "fcda6141-233b-5b1f-8d1d-ec0af7926402",
            ),
            (
                "release, from a MusicBrainz id",
                release.to_string(),
                "1f4e63fd-2605-557d-ba5b-05ae0c641352",
            ),
            // **The one that actually changed**, and the one nothing was
            // watching: `of_basis` mints recordings, and a recording is what
            // every rating and play in the log hangs off.
            (
                "recording, named by MusicBrainz",
                recording.to_string(),
                "8e86783f-0e86-58c8-aa82-f0b080b06a2c",
            ),
            (
                "recording, named by another authority",
                Id::<Recording>::of_basis(&Basis::External {
                    authority: "beatport".into(),
                    key: "12345".into(),
                })
                .to_string(),
                "80e0595d-a335-5edc-8cb1-cbe9675cb84e",
            ),
            (
                "recording, resting on its bytes",
                Id::<Recording>::of_basis(&Basis::Audio(a)).to_string(),
                "0a565b68-a9b2-5d5b-bf99-874166cb19b4",
            ),
            (
                "artist, from a MusicBrainz id",
                Id::<Artist>::of_artist(Some(mbid), "ignored").to_string(),
                "9154d623-5bb4-51db-b1fd-8691d2674bad",
            ),
            (
                "artist, from a name",
                Id::<Artist>::of_artist(None, "  Stephan   Bodzin  ").to_string(),
                "4d18f179-cb5d-5244-b07a-3db6d1c704a3",
            ),
            (
                "release, falling back to its group",
                Id::<Release>::of_release(None, group).to_string(),
                "7e00a5cf-4a80-512d-85d9-7d8da8f57e4c",
            ),
            (
                "medium, from its position",
                medium.to_string(),
                "bb9fe36a-6317-5928-8e83-2af13e66f3e0",
            ),
            (
                "track, from the recording filling it",
                Id::<Track>::of_track(medium, recording).to_string(),
                "b897335e-2065-595f-a90d-bf4b945d4a1d",
            ),
            // These four go through `ingest::normalise`, so a change there
            // moves all of them at once — which is the second thing that
            // regenerated this library's log.
            //
            // **The inputs are deliberately untidy.** A first version used
            // `"Dub Techno"` and `"performer"`, which normalise to themselves,
            // so reverting the collapse of internal whitespace changed nothing
            // and the pin proved nothing about it. Each carries the shape the
            // rule is *for*: a doubled space, surrounding space, a capital, a
            // tab.
            // **Unchanged across #117**, and that is the point: the genre
            // facet's key is the string `Genre::KIND` was, so making the facet
            // the domain separator re-mints nothing.
            (
                "genre concept",
                Id::<Concept>::in_facet(facet::GENRE, "Dub  Techno").to_string(),
                "70a98a74-2ef9-5ce6-95fb-c0aa48472f53",
            ),
            // **Changed, once, deliberately.** Was
            // `940e7f75-036b-50cc-b881-de887c835e4a`, seeded from the facet
            // key `qualifier`; renaming the axis to `version` (#117) re-mints
            // every concept on it. Free when it was done — the live log held
            // no version fact at all, measured — and it is exactly the change
            // this pin exists to make somebody argue for.
            (
                "version concept",
                Id::<Concept>::in_facet(facet::VERSION, "  Instrumental  ").to_string(),
                "c3bfce70-6a45-535c-9f19-1bc91c6c6909",
            ),
            (
                "credit role",
                Id::<Role>::of_name("Performer").to_string(),
                "30cad611-d34c-5fcf-a07c-29f2073bca18",
            ),
            (
                "imprint",
                Id::<Label>::of_name("Herzblut\tRecords").to_string(),
                "7fb47fe3-0670-5f7f-aa73-d28bf2804b68",
            ),
        ];

        for (what, got, want) in pinned {
            assert_eq!(
                got, want,
                "minting changed for {what} — every stored id of that kind is \
                 now wrong, and every fact naming one points at nothing"
            );
        }
    }
}
