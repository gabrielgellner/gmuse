//! **What this project has promised not to re-mint** — #213, and #66 before it.
//!
//! The derivation moved to `gmuse-ids`; this table did not follow it, because
//! it pins ids seeded from both sides of that boundary. A facet key is a
//! vocabulary term and lives in [`crate::model::facet`]; `Id::derived` and the
//! `KIND` strings live in the ids crate. Only here are both in scope at once,
//! and one table showing every pinned id is worth more than two halves each
//! showing the ids whose inputs happened to be local.

#[cfg(test)]
mod tests {
    use crate::audio_id::AudioId;
    use crate::model::entity::Concept;
    use crate::model::entity::{Release, ReleaseGroup, Work};
    use crate::model::facet;
    use crate::model::id::Id;
    use uuid::Uuid;

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
