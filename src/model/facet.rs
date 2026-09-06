//! Axes of description, and the terms on them — issue #117.
//!
//! Genre and version were two copies of one structure. This is the structure:
//! **a facet is an axis, a concept is a term on exactly one axis, and an
//! assertion is a sourced claim that a term describes a subject.**
//!
//! ## Why an axis, and not a deeper hierarchy
//!
//! Measured on the real library: no genre predicate reproduces the `Dance`
//! folder. The closest carries 876 false positives, because Laurie Anderson,
//! Beck, Enya and DEVO are all correctly `Electronic` and none of them is what
//! that folder means. That is not a tagging failure but a category error.
//! Cataloguing separates **is-ness** — what a thing is, `techno` — from
//! **for-ness** — what it is for, music to dance to. Ranganathan's colon
//! classification exists because a hierarchy cannot carry two axes, and
//! putting `Dance` into the genre tree degrades exactly the way faceted
//! classification was invented to prevent.
//!
//! ISO 25964's definition is the operative one: a facet is a grouping of
//! concepts of the same inherent category **within one vocabulary**. One
//! concept list, faceted — so ids cannot collide and the machinery is shared.
//!
//! ## The facet is on the concept, not on the edge
//!
//! Some labels live on two axes. `ambient` is a style and also a function —
//! Eno coined it as a function. `christmas` is an occasion wearing a genre's
//! clothes. Two ways to model that:
//!
//! - **on the concept** — style-`ambient` and function-`ambient` are separate
//!   ids that happen to share a label. A **split**, repairable by a merge.
//! - **on the edge** — one concept attached two ways. Separating them later
//!   means splitting an id already written across the log.
//!
//! `prefer the repairable error` decides it, so the facet goes in the seed.
//! The two are homographs — same spelling, unrelated senses — and authority
//! control has handled those for a century by making the label insufficient
//! for identity: `Mercury (planet)`, `Mercury (element)`. Here the facet plays
//! the parenthetical's part. The identity is not `ambient`, it is
//! `(genre, ambient)`.
//!
//! ## What must not become a facet
//!
//! - **A collection.** SKOS makes `Concept` and `Collection` disjoint and
//!   forbids `broader` into a collection, for the reason #90 recorded: two
//!   `Dance`s free to drift. You *filter by* a concept and *resolve* a
//!   collection to its members.
//! - **A measurement.** Tempo, bitrate, year, duration: continuous, no
//!   hierarchy, no curation. They stay fields with comparison operators. The
//!   moment `bpm` acquires a hierarchy, somebody has invented a genre.

use crate::model::entity::{Concept, Facet, Recording, ReleaseGroup};
use crate::model::id::Id;

/// What an assertion is *about*.
///
/// **Was `GenreOn`, and the rename is the point** (#117): the same subject
/// serves every axis, because "what is this described by" is one question
/// however many axes answer it.
///
/// The release group is the default for genre, because that is where the data
/// is — querying the release found genres for 3 of 49 records, the group for
/// 130 of 429. A version is asserted on the recording, since 7.3% of this
/// library has no work and a fact on the work link could not mark an
/// instrumental white label at all.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Described {
    ReleaseGroup(Id<ReleaseGroup>),
    Recording(Id<Recording>),
}

/// An axis, as the fold has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FacetRecord {
    /// What concepts on this axis seed from, and what the query language calls
    /// it: `genre=techno` finds the facet whose key is `genre`.
    pub key: String,
    pub label: String,
}

/// A term, as the fold has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConceptRecord {
    pub facet: Id<Facet>,
    pub label: String,
    /// **A node label is not assignable** — ISO 25964's name for a term put
    /// into an array purely to organise it.
    ///
    /// `mix-variant` exists to be closed over — `version+ = mix-variant` — and
    /// asserting it directly says nothing about a recording. The standard's
    /// rule comes with the standard's name: a node label does not take part in
    /// retrieval, it only groups.
    pub node_label: bool,
}

/// What kind of music this is. `techno`, `ambient`, `bluegrass`.
pub const GENRE: &str = "genre";

/// Which version of a song you are holding. `live`, `instrumental`.
///
/// **Called `qualifier` until #117.** Two reasons to rename it. In authority
/// control a *qualifier* is the parenthetical that disambiguates a homograph,
/// which is now the facet's job, so the word was doubly claimed. And the old
/// module's own definition already named the right word: "a qualifier says
/// *which version* of it you are holding". `version=instrumental` reads;
/// `qualifier=instrumental` needs a gloss.
pub const VERSION: &str = "version";

/// What music is *for* — `dance-floor`, `background`, `ceremony`.
///
/// **The axis the `Dance` folder was on all along**, and the one genre could
/// not carry. Measured, the closest genre predicate to that folder had 876
/// false positives: `Electronic` is is-ness and the folder is for-ness, so
/// Enya is correctly one and emphatically not the other.
///
/// It asserts nothing on its own — a facet is an axis, and what is *on* it is
/// somebody's claim. Whether a recording earns `dance-floor` stays a curation
/// question, and what belongs to the Dance collection stays a separate one
/// (#90): a facet is a collection development policy, not the holdings.
pub const FUNCTION: &str = "function";

/// What key a recording is in — `Am`, `C#m`, `F` (#136).
///
/// **A claim, not a measurement**, and the distinction the exclusion list
/// above turns on is not "was it detected". Tempo is continuous and answers to
/// `<` and `>`; a key is one of twenty-four names and answers to neither. What
/// decides it is that **key detectors disagree** — two tools give two answers
/// about one file, which is the defining property of a claim here and the same
/// reason genre is one. As a field it would be last-write-wins, and a rescan
/// would silently overwrite somebody's correction.
///
/// The terms on it are closed and uncurated, so this axis has no hierarchy and
/// wants none: which keys mix is arithmetic on the circle of fifths
/// ([`crate::model::key`]), and writing that as edges would fill a store built
/// for contestable claims with rows nobody could ever retract.
pub const KEY: &str = "key";

/// Every axis, and what to call it.
///
/// **A closed list, where concepts on an axis are open**, and the promotion
/// test in `vocabulary.rs` is why. Adding a *concept* needs no behaviour
/// written for it — that is what makes a genre data. Adding an *axis* needs a
/// query field name and a place in completion, which is behaviour, however
/// little. It is also an editorial act: facet analysis is a design decision
/// about how a collection is described, not something a tag should be able to
/// invent.
///
/// So a new axis costs **one line here** — no fact, no fold arm, no map, no
/// entity kind, no match arm. `FacetDefined` still exists so a facet's label
/// is in the log and syncs; this is what the parser knows without one.
pub const ALL: &[(&str, &str)] = &[
    (GENRE, "Genre"),
    (VERSION, "Version"),
    (FUNCTION, "Function"),
    (KEY, "Key"),
];

/// The axis this name refers to, if any names it.
///
/// **Where an unknown field stops being a query.** `genrre=techno` is a typo
/// and has to say so; resolving any word to a facet id would make it a query
/// that quietly matches nothing, which is the failure this project keeps
/// turning into a declaration.
pub fn key_of(name: &str) -> Option<&'static str> {
    ALL.iter()
        .find(|(key, _)| *key == name)
        .map(|(key, _)| *key)
}

impl Id<Facet> {
    /// The id a facet's key always mints.
    pub fn of_key(key: &str) -> Self {
        Id::derived(key.trim().to_ascii_lowercase().as_bytes())
    }
}

impl Id<Facet> {
    /// The array this axis's edges live in, unless somebody defines another.
    ///
    /// A facet may carry several hierarchies — a browse one and a filter one
    /// over the same list, as SDMX does — so this is *a* hierarchy rather than
    /// *the* hierarchy. It exists because the seeded vocabularies need
    /// somewhere to put their edges without inventing a name each.
    pub fn array(key: &str) -> Id<crate::model::entity::Hierarchy> {
        Id::derived(key.trim().to_ascii_lowercase().as_bytes())
    }
}

impl Id<Concept> {
    /// The id a label always mints **on one axis**.
    ///
    /// **The facet is the domain separator**, which is why this does not go
    /// through [`Id::derived`] and its `Entity::KIND`. A concept's namespace
    /// is its facet — that is what the type is *for* — so seeding with
    /// `Concept::KIND` would put every axis back in one namespace and let
    /// `ambient` the style collide with `ambient` the function.
    ///
    /// Two properties fall out. Cross-facet collision is impossible. And
    /// because the genre facet's key is the string `Genre::KIND` used to be,
    /// **every genre id ever minted keeps its exact value** — pinned by a test
    /// below, for the reason `NAMESPACE` is pinned: a change here re-mints a
    /// library silently.
    ///
    /// Normalised like every other named concept, so two devices cataloguing
    /// `Techno` and `techno` independently arrive at one id with no registry
    /// between them.
    pub fn in_facet(facet_key: &str, label: &str) -> Self {
        let mut seed = Vec::new();
        seed.extend_from_slice(facet_key.trim().to_ascii_lowercase().as_bytes());
        seed.push(0x1f); // the separator `Id::derived` uses, same reason
        seed.extend_from_slice(crate::model::ingest::normalise(label).as_bytes());
        Id::from_uuid(uuid::Uuid::new_v5(&crate::model::id::NAMESPACE, &seed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::reference::Source;

    /// **The migration is a no-op on disk**, and stays one.
    ///
    /// The genre facet's key is the string that was `Genre::KIND`, so a
    /// concept on it mints exactly the id `Id::<Genre>::of_name` minted before
    /// #117. Nothing re-mints and no library is silently re-keyed.
    ///
    /// **Pinned as literals rather than computed**, for the reason `NAMESPACE`
    /// is: a test that derives the expected value the same way the code does
    /// agrees with any change, including the wrong one. These three were
    /// minted by the code this replaced.
    #[test]
    fn a_genre_keeps_the_id_it_always_had() {
        for (label, want) in [
            ("techno", "c31dbeb6-90c8-5ce4-a409-d6ad0e19cd91"),
            ("Deep House", "f720a51f-eb10-5643-85a2-2b30c01d29f6"),
            (" Ambient ", "eb37d79c-5fc2-5eae-b21e-4b31c5c41687"),
        ] {
            assert_eq!(
                Id::<Concept>::in_facet(GENRE, label).as_uuid().to_string(),
                want,
                "{label} would re-mint, and every log naming it would go stale"
            );
        }
    }

    /// **The homograph, which is the reason the facet is in the seed.**
    ///
    /// `ambient` names a style and, since Eno, a function. Two concepts that
    /// share a label, not one concept on two axes — so they are two ids, and
    /// asserting one says nothing about the other.
    #[test]
    fn one_label_on_two_axes_is_two_concepts() {
        assert_ne!(
            Id::<Concept>::in_facet(GENRE, "ambient"),
            Id::<Concept>::in_facet("function", "ambient"),
            "a style and a function collapsed into one concept"
        );
    }

    /// Convergent within a facet, as every named concept here is.
    #[test]
    fn spelling_and_padding_do_not_split_a_concept() {
        assert_eq!(
            Id::<Concept>::in_facet(GENRE, "Techno"),
            Id::<Concept>::in_facet(GENRE, " techno ")
        );
        // And the facet key is normalised the same way, so a config or a
        // proposal file written in caps reaches the same axis.
        assert_eq!(Id::<Facet>::of_key("Genre"), Id::<Facet>::of_key("genre"));
    }

    /// **The test of whether any of this worked** — #117's own acceptance
    /// criterion.
    ///
    /// A third axis, and nothing was written for it: no fact, no fold arm, no
    /// map, no entity kind. `function` is a string, and everything the two
    /// built-in axes have — convergent ids, a catalogued label, a sourced and
    /// retractable claim, a facet-scoped reader — it has too.
    ///
    /// This is what genre could not do. Measured, the closest genre predicate
    /// to the `Dance` folder carried 876 false positives, because `Electronic`
    /// is is-ness and the folder is for-ness. Now for-ness has somewhere to
    /// live that is not a corner of the genre tree.
    #[test]
    fn a_new_axis_costs_no_facts_and_no_fold_code() {
        const FUNCTION: &str = "function";
        let recording = Id::<crate::model::entity::Recording>::derived(b"a track");
        let subject = Described::Recording(recording);

        let mut i = Instance::new(Id::derived(b"laptop"));
        i.assert_concept(
            100,
            subject,
            FUNCTION,
            "dance-floor",
            Source::Manual,
            Confidence::Confirmed,
        );
        // And the same recording keeps its style, on its own axis.
        i.assert_concept(
            100,
            subject,
            GENRE,
            "ambient",
            Source::TagVariant,
            Confidence::Provisional,
        );

        let cat = i.catalogue();
        assert_eq!(cat.labels_on(subject, FUNCTION), ["dance-floor"]);
        assert_eq!(
            cat.labels_on(subject, GENRE),
            ["ambient"],
            "a function leaked into the genre column, which is the failure \
             this whole issue is about"
        );

        // **The homograph, end to end.** Asserting `ambient` as a function
        // says nothing about `ambient` the style, and retracting one leaves
        // the other standing.
        i.assert_concept(
            200,
            subject,
            FUNCTION,
            "ambient",
            Source::Manual,
            Confidence::Confirmed,
        );
        i.retract_concept(300, subject, FUNCTION, "ambient", Source::Manual);
        let cat = i.catalogue();
        assert_eq!(
            cat.labels_on(subject, GENRE),
            ["ambient"],
            "retracting the function retracted the style with it"
        );
        assert_eq!(cat.labels_on(subject, FUNCTION), ["dance-floor"]);
    }
}
