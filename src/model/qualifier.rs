//! Which version of a song this is — issue #101.
//!
//! A qualifier is **an assertion about a recording, drawn from a small concept
//! scheme, carrying a basis and a confidence**. Not a field, not an enum, not
//! an attribute on the work link.
//!
//! ## Why the recording, and not the work link
//!
//! MusicBrainz hangs `instrumental` on the recording→work relationship, and
//! that placement cannot be copied here because it costs something MusicBrainz
//! does not pay: **7.3% of this library has no work**, and some never will. A
//! fact on the work link cannot mark an instrumental white label at all. A
//! fact on the recording marks it like anything else.
//!
//! The objection — that "instrumental" is meaningless without a work —
//! overstates. It is meaningful relative to the work *as understood*: `212` is
//! known to have lyrics without a `Work` row existing anywhere. What the
//! missing link costs is only the *inference* that a vocal sibling exists.
//!
//! ## A separate scheme from genre
//!
//! Sharing a list would make `genre = instrumental` expressible, and it is not
//! a genre. The two answer different questions and are curated from different
//! evidence: a genre says what kind of music this is, a qualifier says which
//! version of it you are holding.
//!
//! ## Set-valued by construction
//!
//! A recording can carry `{live, instrumental}`. That falls out of assignments
//! being rows rather than a field, and it is why the query language's default
//! quantification has to be stated rather than discovered.

use crate::model::entity::Qualifier;
use crate::model::id::Id;

/// The concepts this scheme seeds with.
///
/// **Grouping concepts are marked, and are not assignable.** `mix-variant` and
/// `performance-variant` exist to be closed over — `qualifier+ = mix-variant`
/// — and asserting one directly says nothing about a recording. That is SKOS's
/// Concept-versus-Collection distinction, and the seed marks it rather than
/// leaving `qualifier = mix-variant` to return an empty set and look like an
/// answer.
pub const SEED: &[(&str, Option<&str>, bool)] = &[
    // (label, parent, assignable)
    ("mix-variant", None, false),
    ("instrumental", Some("mix-variant"), true),
    ("acappella", Some("mix-variant"), true),
    ("clean", Some("mix-variant"), true),
    ("dub-plate", Some("mix-variant"), true),
    ("performance-variant", None, false),
    ("live", Some("performance-variant"), true),
    ("demo", Some("performance-variant"), true),
    ("partial", None, true),
];

impl Id<Qualifier> {
    /// The id a concept label always mints. Convergent for the reason
    /// `Id::<Genre>::of_name` is, and by the same rule.
    pub fn of_label(label: &str) -> Self {
        Id::derived(label.trim().to_lowercase().as_bytes())
    }
}

/// Whether this concept may be asserted about a recording.
///
/// A grouping concept may not. Refusing is better than accepting and folding
/// to nothing, because an assignment nobody can see is indistinguishable from
/// one nobody made.
pub fn assignable(label: &str) -> bool {
    let wanted = label.trim().to_lowercase();
    SEED.iter()
        .find(|(name, _, _)| *name == wanted)
        .is_none_or(|(_, _, ok)| *ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grouping concept is a place in the tree, not a claim about a song.
    #[test]
    fn a_grouping_concept_cannot_be_asserted() {
        assert!(!assignable("mix-variant"));
        assert!(!assignable("performance-variant"));
        assert!(assignable("instrumental"));
        assert!(assignable("live"));
        assert!(assignable("partial"), "a leaf at top level is still a leaf");
    }

    /// A concept nobody seeded is assumed assignable: the scheme grows by
    /// fact, so refusing what is not in the seed would freeze the vocabulary.
    #[test]
    fn an_unseeded_concept_is_assignable() {
        assert!(assignable("karaoke"));
    }

    /// Convergent, and case-folded, exactly as genre is.
    #[test]
    fn one_label_one_concept() {
        assert_eq!(
            Id::<Qualifier>::of_label("Instrumental"),
            Id::<Qualifier>::of_label(" instrumental ")
        );
        assert_ne!(
            Id::<Qualifier>::of_label("live"),
            Id::<Qualifier>::of_label("demo")
        );
    }

    /// Every seeded parent is itself seeded, or the closure has a dangling
    /// edge the moment the scheme is written to the log.
    #[test]
    fn every_parent_is_a_concept() {
        let names: Vec<&str> = SEED.iter().map(|(n, _, _)| *n).collect();
        for (name, parent, _) in SEED {
            if let Some(p) = parent {
                assert!(
                    names.contains(p),
                    "{name} names a parent that is not seeded: {p}"
                );
            }
        }
    }
}

#[cfg(test)]
mod fold_tests {
    use crate::model::entity::{Device, Qualifier, Recording};
    use crate::model::id::Id;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::reference::Source;

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn rec() -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(212))
    }

    fn live(i: &Instance, label: &str) -> bool {
        i.catalogue()
            .qualifiers
            .get(&(rec(), Id::<Qualifier>::of_label(label)))
            .is_some_and(Option::is_some)
    }

    /// A recording can be two things at once, which a field could not say.
    #[test]
    fn a_recording_carries_several_qualifiers() {
        let mut i = Instance::new(device(1));
        i.assign_qualifier(
            100,
            rec(),
            "live",
            Source::TagVariant,
            Confidence::Confirmed,
        );
        i.assign_qualifier(
            200,
            rec(),
            "instrumental",
            Source::TagVariant,
            Confidence::Confirmed,
        );
        assert!(live(&i, "live") && live(&i, "instrumental"));
    }

    /// **The correction survives the sync**, which is the whole reason a
    /// retraction is a fact rather than a delete. An import asserts, a person
    /// disagrees, and a stale replica re-delivering the import cannot
    /// resurrect it — the retraction carries the later stamp.
    #[test]
    fn a_retraction_is_not_undone_by_a_replica() {
        let mut mine = Instance::new(device(1));
        mine.assign_qualifier(
            100,
            rec(),
            "instrumental",
            Source::Discogs,
            Confidence::Matched,
        );
        let import = mine.records().clone();

        mine.retract_qualifier(200, rec(), "instrumental");
        assert!(!live(&mine, "instrumental"), "the retraction did not take");

        // The other machine still holds only the import and sends it back.
        mine.merge(300, &import);
        assert!(
            !live(&mine, "instrumental"),
            "a stale replica resurrected a retracted qualifier"
        );
    }

    /// **"Decided not to apply" is not "nobody said", and the fold has to keep
    /// them apart.**
    ///
    /// A retraction leaves the pair present and empty rather than removing it.
    /// Both spellings fold to the same answer for `is this live?`, which is why
    /// a test asking only that passed against either — but the query language
    /// needs three states, not two: `qualifier = live`, `not qualifier = live`,
    /// and `not qualifier?` for the review queue. Only a row that exists and is
    /// empty can say the middle one.
    #[test]
    fn a_retraction_is_a_decision_and_not_a_silence() {
        let mut i = Instance::new(device(1));
        i.assign_qualifier(100, rec(), "live", Source::Discogs, Confidence::Matched);
        i.retract_qualifier(200, rec(), "live");

        let cat = i.catalogue();
        let decided = cat
            .qualifiers
            .get(&(rec(), Id::<Qualifier>::of_label("live")));
        assert_eq!(
            decided,
            Some(&None),
            "a retraction has to leave the pair present and empty — removing it \
             makes a decision indistinguishable from a silence"
        );
        assert_eq!(
            cat.qualifiers
                .get(&(rec(), Id::<Qualifier>::of_label("instrumental"))),
            None,
            "and something nobody has ruled on has no row at all"
        );
    }

    /// And a later assertion still wins, or a retraction would be permanent —
    /// which is a delete wearing a different name.
    #[test]
    fn a_later_assertion_undoes_a_retraction() {
        let mut i = Instance::new(device(1));
        i.assign_qualifier(100, rec(), "live", Source::Discogs, Confidence::Matched);
        i.retract_qualifier(200, rec(), "live");
        i.assign_qualifier(300, rec(), "live", Source::Manual, Confidence::Confirmed);
        assert!(live(&i, "live"));
    }

    /// The import stays in the log after a retraction. Deleting it would
    /// destroy the provenance that says where the claim came from.
    #[test]
    fn a_retraction_keeps_what_it_shadows() {
        let mut i = Instance::new(device(1));
        i.assign_qualifier(
            100,
            rec(),
            "instrumental",
            Source::Discogs,
            Confidence::Matched,
        );
        i.retract_qualifier(200, rec(), "instrumental");
        let kinds: Vec<_> = i
            .records()
            .iter()
            .filter(|r| format!("{:?}", r.fact).starts_with("QualifierAssigned"))
            .collect();
        assert_eq!(kinds.len(), 1, "the shadowed assertion was thrown away");
    }

    /// A grouping concept is refused rather than accepted and folded to
    /// nothing, because an assignment nobody can see reads as one nobody made.
    #[test]
    fn a_grouping_concept_is_refused_at_the_door() {
        let mut i = Instance::new(device(1));
        let taken = i.assign_qualifier(
            100,
            rec(),
            "mix-variant",
            Source::Manual,
            Confidence::Confirmed,
        );
        assert!(!taken);
        assert!(i.records().is_empty(), "a refused assignment still wrote");
    }

    /// Seeding twice writes the scheme once: every id derives from its label.
    #[test]
    fn seeding_is_idempotent() {
        let mut i = Instance::new(device(1));
        i.seed_qualifiers(100);
        let after_one = i.catalogue().qualifier_labels.len();
        i.seed_qualifiers(200);
        assert_eq!(i.catalogue().qualifier_labels.len(), after_one);
        assert!(i.catalogue().qualifier_narrows.len() >= 6);
    }
}
