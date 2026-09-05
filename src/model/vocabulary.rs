//! Genre as a codelist — issue #74, `domain-model.md` §2.
//!
//! Genre is the one vocabulary in this model that is **data**, by the
//! promotion test: adding a value needs no behaviour written for it, only
//! grouping and filtering. So it gets identity, aliases, hierarchy, merge and
//! external bindings — everything an entity gets — where a musical key, which
//! needs its position on the circle of fifths written before it can exist, does
//! not.
//!
//! ## What this promotes
//!
//! `tools/genre/propose.py` holds a real concept scheme in Python
//! dictionaries that gmuse cannot see: **81 assertions** — 53 `IMPLIES`
//! (narrower-of), 20 `BROAD` (top concepts), 3 `RENAME` (alternate labels) and
//! 5 `DROP` (not genres at all). Moving them here is what stops the compound
//! `Broad; Style` string from being load-bearing.
//!
//! ## The hierarchy is a separate artefact
//!
//! As in SDMX, and unlike SKOS's `broader` on the concept itself. One genre
//! list can carry a browse hierarchy and a filter hierarchy at the same time
//! without the concepts changing — and a concept can therefore be moved in one
//! hierarchy without being moved in the other.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::entity::{Genre, Hierarchy};
use crate::model::id::{Id, Ref};
use crate::model::reference::Source;

impl Id<Genre> {
    /// The id a genre name always mints, on any machine.
    ///
    /// **Convergent, because two devices tagging "Techno" must agree without
    /// meeting** — and there is no external authority for a genre the way
    /// MusicBrainz is one for a recording, so the name is what there is.
    ///
    /// Folded to lowercase and trimmed, so `Techno`, `techno` and ` Techno `
    /// are one concept. Nothing else is normalised: `Deep House` and
    /// `deep-house` stay apart, because collapsing them is a judgement about
    /// the vocabulary rather than about whitespace, and a wrong collapse is a
    /// merge — the error this model cannot repair.
    ///
    /// The consequence is honest and named in the docs above: **the id is
    /// derived from the label, so renaming a concept mints a new one.** That
    /// is what `Merges<Genre>` is for, and it is the same trade `Basis::Audio`
    /// makes for a recording — convergence now, a repairable split later.
    /// The id a name always mints.
    ///
    /// **Through [`crate::model::ingest::normalise`] and nothing else**, which
    /// is what lets two devices define a concept independently and union into
    /// one row with no registry — the same bet recording ids make. It was
    /// spelled out here and again in `Vocabulary::define`, agreeing only by
    /// coincidence; two spellings of the identity rule is one waiting to drift.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::model::ingest::normalise(name).as_bytes())
    }
}

/// A concept in the vocabulary. The id is meaningless; the label is mutable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GenreRecord {
    pub id: Id<Genre>,
    pub name: String,
}

/// One grouping of the concepts. A list may carry several.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HierarchyRecord {
    pub id: Id<Hierarchy>,
    pub name: String,
}

/// One narrower-of edge, in one hierarchy.
///
/// Row existence, so it unions across devices and there is no
/// `Genre.children` to collide over.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct HierarchyEdge {
    pub hierarchy: Ref<Hierarchy>,
    pub parent: Ref<Genre>,
    pub child: Ref<Genre>,
}

/// What a genre is assigned to.
///
/// The release group by default, because that is how this library is curated
/// and where the data lives: querying the release found genres for 3 of 49
/// records, the group for 130 of 429.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum GenreOn {
    ReleaseGroup(Id<crate::model::entity::ReleaseGroup>),
    Recording(Id<crate::model::entity::Recording>),
}

/// One genre on one subject, with why it was assigned and how confidently.
///
/// A bare reference would have nowhere to record either, which is exactly what
/// `genre-uncertain.tsv` tracks today and throws away at the end of every pass.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GenreAssignment {
    pub subject: GenreOn,
    pub genre: Ref<Genre>,
    pub basis: Source,
    pub confidence: crate::model::ingest::Confidence,
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — cycle detection in the genre hierarchy, which no import path runs"
)]
/// A cycle someone's data acquired without anyone authoring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cycle {
    pub hierarchy: Id<Hierarchy>,
    pub at: Id<Genre>,
}

/// The vocabulary, its hierarchies, and what has been assigned from it.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Vocabulary {
    pub genres: BTreeMap<Id<Genre>, GenreRecord>,
    pub hierarchies: BTreeMap<Id<Hierarchy>, HierarchyRecord>,
    pub edges: BTreeSet<HierarchyEdge>,
    pub assignments: Vec<GenreAssignment>,
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — cycle detection in the genre hierarchy, which no import path runs"
)]
/// How deep a hierarchy walk may go before it is called a cycle.
const MAX_DEPTH: usize = 16;

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — cycle detection in the genre hierarchy, which no import path runs"
)]
impl Vocabulary {
    pub fn define(&mut self, name: &str) -> Id<Genre> {
        let id = Id::<Genre>::of_name(name);
        self.genres.entry(id).or_insert_with(|| GenreRecord {
            id,
            name: name.to_string(),
        });
        id
    }

    pub fn hierarchy(&mut self, name: &str) -> Id<Hierarchy> {
        let id = Id::<Hierarchy>::derived(name.as_bytes());
        self.hierarchies
            .entry(id)
            .or_insert_with(|| HierarchyRecord {
                id,
                name: name.to_string(),
            });
        id
    }

    /// `child` is narrower than `parent`, in this hierarchy.
    pub fn narrows(&mut self, hierarchy: Id<Hierarchy>, child: Id<Genre>, parent: Id<Genre>) {
        self.edges.insert(HierarchyEdge {
            hierarchy: Ref::to(hierarchy),
            parent: Ref::to(parent),
            child: Ref::to(child),
        });
    }

    /// Everything at or under a concept, in one hierarchy.
    ///
    /// This is what `genre="*electronic*"` has been faking: a substring match
    /// that finds `Electronic` inside `Electronic; Techno` by luck of spelling
    /// rather than by meaning.
    pub fn under(&self, hierarchy: Id<Hierarchy>, root: Id<Genre>) -> BTreeSet<Id<Genre>> {
        let mut seen = BTreeSet::from([root]);
        let mut frontier = vec![root];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for parent in frontier.drain(..) {
                for e in &self.edges {
                    if e.hierarchy.id() == hierarchy && e.parent.id() == parent {
                        let child = e.child.id();
                        if seen.insert(child) {
                            next.push(child);
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        seen
    }

    /// The concepts with no parent — the top of a hierarchy.
    pub fn top_of(&self, hierarchy: Id<Hierarchy>) -> BTreeSet<Id<Genre>> {
        let children: BTreeSet<_> = self
            .edges
            .iter()
            .filter(|e| e.hierarchy.id() == hierarchy)
            .map(|e| e.child.id())
            .collect();
        self.edges
            .iter()
            .filter(|e| e.hierarchy.id() == hierarchy)
            .map(|e| e.parent.id())
            .filter(|p| !children.contains(p))
            .collect()
    }

    /// Cycles, so the data gets **fixed** rather than merely survived.
    ///
    /// Edges are rows and rows union, so one device adding `A narrows B` while
    /// another adds `B narrows A` produces a cycle neither authored. The depth
    /// bound in [`Vocabulary::under`] stops that hanging a render, which is not
    /// the same as anyone knowing about it.
    pub fn cycles(&self) -> Vec<Cycle> {
        let mut found = Vec::new();
        for h in self.hierarchies.keys() {
            for g in self.genres.keys() {
                if self.under(*h, *g).len() > 1
                    && self
                        .under(*h, *g)
                        .iter()
                        .any(|d| *d != *g && self.under(*h, *d).contains(g))
                {
                    found.push(Cycle {
                        hierarchy: *h,
                        at: *g,
                    });
                }
            }
        }
        found
    }

    /// What a recording's genres actually are.
    ///
    /// Assignments are rows and rows only union, so "recording-level overrides"
    /// is a **read rule** rather than something the storage does — and it has
    /// to be written down or every caller invents its own.
    ///
    /// The middle rung is where the compilation case lives: a rock song on its
    /// album *and* on _80s Hits_ is reached through two release groups, and a
    /// naive union files it under the compilation's `Pop`. Preferring
    /// non-compilations resolves it with data the model already carries;
    /// falling back to all of them keeps a compilation-only recording from
    /// having no genre at all.
    pub fn effective_genres(
        &self,
        recording: Id<crate::model::entity::Recording>,
        groups: &[(Id<crate::model::entity::ReleaseGroup>, bool)],
    ) -> BTreeSet<Id<Genre>> {
        let own: BTreeSet<_> = self
            .assignments
            .iter()
            .filter(|a| a.subject == GenreOn::Recording(recording))
            .map(|a| a.genre.id())
            .collect();
        if !own.is_empty() {
            return own;
        }

        let from = |only_albums: bool| -> BTreeSet<Id<Genre>> {
            self.assignments
                .iter()
                .filter_map(|a| match a.subject {
                    GenreOn::ReleaseGroup(g)
                        if groups
                            .iter()
                            .any(|(id, comp)| *id == g && (!only_albums || !*comp)) =>
                    {
                        Some(a.genre.id())
                    }
                    _ => None,
                })
                .collect()
        };

        let albums = from(true);
        if albums.is_empty() {
            from(false)
        } else {
            albums
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::{Recording, ReleaseGroup};
    use crate::model::ingest::Confidence;

    /// The vocabulary `tools/genre/propose.py` already holds, in miniature.
    fn library_vocabulary() -> (Vocabulary, Id<Hierarchy>, Id<Genre>, Id<Genre>) {
        let mut v = Vocabulary::default();
        let browse = v.hierarchy("browse");
        let electronic = v.define("Electronic");
        let techno = v.define("Techno");
        let minimal = v.define("Minimal Techno");
        v.narrows(browse, techno, electronic);
        v.narrows(browse, minimal, techno);
        (v, browse, electronic, techno)
    }

    /// **What the compound string was faking.** `genre="*electronic*"` finds
    /// `Electronic; Techno` by luck of spelling; this finds it by meaning, and
    /// finds `Minimal Techno` too, which the substring never did.
    #[test]
    fn filtering_under_a_concept_reaches_the_whole_subtree() {
        let (v, browse, electronic, techno) = library_vocabulary();
        let under = v.under(browse, electronic);
        assert_eq!(under.len(), 3, "itself, Techno, and Minimal Techno");
        assert!(under.contains(&techno));
        assert_eq!(v.under(browse, techno).len(), 2, "and it narrows downward");
    }

    /// One list, several hierarchies — the SDMX move. A browse tree and a
    /// filter tree can disagree without either concept changing.
    #[test]
    fn one_vocabulary_carries_more_than_one_hierarchy() {
        let (mut v, browse, electronic, techno) = library_vocabulary();
        let dancefloor = v.hierarchy("dancefloor");
        let house = v.define("House");
        v.narrows(dancefloor, techno, house);

        assert!(v.under(browse, electronic).contains(&techno));
        assert!(
            !v.under(dancefloor, electronic).contains(&techno),
            "the other hierarchy says something else, and both are true"
        );
        assert_eq!(v.top_of(dancefloor), BTreeSet::from([house]));
    }

    /// Edges are rows and rows union, so two devices can build a cycle neither
    /// authored. The depth bound stops it hanging a render; this is how anyone
    /// finds out.
    #[test]
    fn a_cycle_nobody_authored_is_detected() {
        let (mut v, browse, electronic, techno) = library_vocabulary();
        assert!(v.cycles().is_empty());

        v.narrows(browse, electronic, techno); // the other device's edge
        assert!(
            !v.cycles().is_empty(),
            "a cycle is a thing to be fixed, not merely survived"
        );
        assert!(
            v.under(browse, electronic).len() <= v.genres.len(),
            "and the walk still terminates"
        );
    }

    /// **The compilation case.** A rock song on its album and on _80s Hits_
    /// must not be filed under the compilation's genre.
    #[test]
    fn a_compilation_does_not_decide_a_track_s_genre() {
        let mut v = Vocabulary::default();
        let rock = v.define("Rock");
        let pop = v.define("Pop");
        let album = Id::<ReleaseGroup>::derived(b"the-album");
        let comp = Id::<ReleaseGroup>::derived(b"80s-hits");
        let song = Id::<Recording>::derived(b"the-song");

        for (subject, genre) in [(album, rock), (comp, pop)] {
            v.assignments.push(GenreAssignment {
                subject: GenreOn::ReleaseGroup(subject),
                genre: Ref::to(genre),
                basis: Source::Discogs,
                confidence: Confidence::Matched,
            });
        }

        let on_both = [(album, false), (comp, true)];
        assert_eq!(
            v.effective_genres(song, &on_both),
            BTreeSet::from([rock]),
            "the album decides, not the compilation"
        );
    }

    /// But a compilation-only track is not left with nothing.
    #[test]
    fn a_compilation_still_answers_when_it_is_all_there_is() {
        let mut v = Vocabulary::default();
        let pop = v.define("Pop");
        let comp = Id::<ReleaseGroup>::derived(b"80s-hits");
        let song = Id::<Recording>::derived(b"only-here");
        v.assignments.push(GenreAssignment {
            subject: GenreOn::ReleaseGroup(comp),
            genre: Ref::to(pop),
            basis: Source::Discogs,
            confidence: Confidence::Matched,
        });

        assert_eq!(
            v.effective_genres(song, &[(comp, true)]),
            BTreeSet::from([pop])
        );
    }

    /// The override, which is what makes the read rule a rule rather than a
    /// guess: where inheritance gets it wrong, an assignment on the recording
    /// settles it.
    #[test]
    fn a_recording_s_own_assignment_wins_outright() {
        let mut v = Vocabulary::default();
        let (rock, jazz) = (v.define("Rock"), v.define("Jazz"));
        let album = Id::<ReleaseGroup>::derived(b"album");
        let song = Id::<Recording>::derived(b"song");
        v.assignments.push(GenreAssignment {
            subject: GenreOn::ReleaseGroup(album),
            genre: Ref::to(rock),
            basis: Source::Discogs,
            confidence: Confidence::Matched,
        });
        v.assignments.push(GenreAssignment {
            subject: GenreOn::Recording(song),
            genre: Ref::to(jazz),
            basis: Source::Manual,
            confidence: Confidence::Confirmed,
        });

        assert_eq!(
            v.effective_genres(song, &[(album, false)]),
            BTreeSet::from([jazz]),
            "the album is not consulted at all once the track has its own"
        );
    }

    /// Multi-genre falls out of row existence rather than needing a list field.
    #[test]
    fn a_subject_can_carry_more_than_one_genre() {
        let mut v = Vocabulary::default();
        let (a, b) = (v.define("Electronic"), v.define("Ambient"));
        let group = Id::<ReleaseGroup>::derived(b"g");
        for g in [a, b] {
            v.assignments.push(GenreAssignment {
                subject: GenreOn::ReleaseGroup(group),
                genre: Ref::to(g),
                basis: Source::Discogs,
                confidence: Confidence::Matched,
            });
        }
        let song = Id::<Recording>::derived(b"s");
        assert_eq!(v.effective_genres(song, &[(group, false)]).len(), 2);
    }

    /// A concept's id comes from its name, so two devices defining `Techno`
    /// independently define the same concept.
    #[test]
    fn defining_a_concept_twice_defines_it_once() {
        let mut v = Vocabulary::default();
        assert_eq!(v.define("Techno"), v.define("techno"));
        assert_eq!(v.genres.len(), 1, "case is not a different concept");
    }

    #[test]
    fn the_vocabulary_round_trips_through_storage() {
        let (v, browse, electronic, _) = library_vocabulary();
        let bytes = postcard::to_allocvec(&v).unwrap();
        let back: Vocabulary = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.under(browse, electronic), v.under(browse, electronic));
    }
}

#[cfg(test)]
mod concept_tests {
    //! **How a concept gets its id, and what a rename costs.**
    //!
    //! Both halves of one bet: an id derived from the label lets two devices
    //! define a concept independently and union into one row with no registry,
    //! and the price is that renaming can split one concept into two. The
    //! split is accepted because it is the error that can be repaired.

    use crate::model::entity::{Device, Genre};
    use crate::model::id::Id;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::merge::Merges;
    use crate::model::reference::{AliasKind, Cataloguable, Source};
    use crate::model::vocabulary::GenreOn;
    use crate::model::{basis::Basis, spine::Recording};

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn rec() -> Id<Recording> {
        Id::of_basis(&Basis::External {
            authority: "test".into(),
            key: "callisto".into(),
        })
    }

    /// **Two devices, two spellings, one concept.** This is what makes the
    /// vocabulary mergeable at all: neither device asks the other what id to
    /// use, and a registry would need one of them to be online.
    #[test]
    fn one_label_is_one_concept_however_it_is_typed() {
        let canonical = Id::<Genre>::of_name("Dub Techno");
        for spelling in ["dub techno", "  DUB TECHNO  ", "dub  techno", "Dub\tTechno"] {
            assert_eq!(
                Id::<Genre>::of_name(spelling),
                canonical,
                "{spelling:?} minted a second concept"
            );
        }
    }

    /// The rule is `ingest::normalise` and nothing else, in both vocabularies.
    ///
    /// It was written out by hand in three places and agreed only by
    /// coincidence; the day one of them learned to strip punctuation, a genre
    /// defined through `Vocabulary::define` and the same genre assigned
    /// through `Instance` would have been different concepts, and nothing
    /// would have said so.
    #[test]
    fn every_concept_id_is_minted_through_one_function() {
        use crate::model::entity::Qualifier;
        use crate::model::ingest::normalise;

        for label in ["Dub Techno", " Deep  House ", "IDM"] {
            assert_eq!(
                Id::<Genre>::of_name(label),
                Id::<Genre>::derived(normalise(label).as_bytes())
            );
            assert_eq!(
                Id::<Qualifier>::of_label(label),
                Id::<Qualifier>::derived(normalise(label).as_bytes())
            );
        }

        // And the two entry points agree, which is the pair that used to be
        // two independent spellings of the same rule.
        let mut v = super::Vocabulary::default();
        assert_eq!(v.define("Dub Techno"), Id::<Genre>::of_name("dub techno"));
    }

    /// **A rename does not re-mint, and the split that follows is repaired
    /// rather than prevented.**
    ///
    /// The whole path: rename here, meet the new label fresh elsewhere, and
    /// end with one concept and both assignments intact. Prevention would cost
    /// either rewriting every assignment on every rename, or giving up derived
    /// ids and with them the ability to define a concept offline.
    #[test]
    fn a_rename_splits_and_a_merge_repairs() {
        let mut mine = Instance::new(device(1));
        let old = Id::<Genre>::of_name("Minimal");
        mine.assign_genre(
            100,
            GenreOn::Recording(rec()),
            "Minimal",
            Source::Discogs,
            Confidence::Matched,
        );
        mine.rename_genre(200, old, "Minimal", "Minimal Techno");

        let cat = mine.catalogue();
        assert_eq!(
            cat.genre_names[&old], "Minimal Techno",
            "the label is a last-writer register and the rename is a write"
        );
        assert!(
            cat.genres
                .contains_key(&(GenreOn::Recording(rec()), old, Source::Discogs)),
            "the assignment followed the id, which is the point of not re-minting"
        );
        assert!(
            cat.aliases.contains(&(
                Cataloguable::Genre(old),
                "Minimal".to_string(),
                AliasKind::Retired
            )),
            "the name it was found under stopped being findable"
        );

        // Elsewhere, somebody meets the new label with no history and mints
        // from it. Nothing is wrong yet — there are simply two rows.
        let fresh = Id::<Genre>::of_name("Minimal Techno");
        assert_ne!(fresh, old, "a rename that re-minted would defeat the test");

        let mut theirs = Instance::new(device(2));
        theirs.assign_genre(
            300,
            GenreOn::Recording(rec()),
            "Minimal Techno",
            Source::Manual,
            Confidence::Confirmed,
        );
        mine.merge(400, theirs.records());
        assert_eq!(
            mine.catalogue()
                .genres
                .keys()
                .filter(|(_, _, _)| true)
                .count(),
            2,
            "the split is real before it is repaired"
        );

        // The repair is the ordinary one, and it is permanent.
        let mut merges = Merges::<Genre>::new();
        merges.merge(fresh, old).expect("a split is mergeable");
        assert_eq!(merges.resolve(fresh), old);
        assert_eq!(merges.resolve(old), old);

        // Both claims survive it, and they are still two claims: a redirect
        // moves rows onto one concept, it does not decide who said what.
        let cat = mine.catalogue();
        let after: Vec<Source> = cat
            .genres
            .keys()
            .map(|(_, g, basis)| {
                assert_eq!(merges.resolve(*g), old, "a row did not resolve");
                *basis
            })
            .collect();
        assert_eq!(after, vec![Source::Discogs, Source::Manual]);
    }

    /// **A concept nobody has placed is an ordinary row.** Assignment does not
    /// wait for placement, or the moment you know the answer is the moment you
    /// cannot record it. What it costs is that closure queries do not reach it,
    /// which is why the list of them is a review query rather than an error.
    #[test]
    fn an_unplaced_concept_is_a_backlog_and_not_an_error() {
        use crate::model::entity::Hierarchy;

        let mut i = Instance::new(device(1));
        let browse = Id::<Hierarchy>::derived(b"browse");
        i.narrows(100, browse, "Electronic", "Techno");
        // A placed concept, so the query has something to *not* return —
        // otherwise it passes on a catalogue that holds one genre.
        i.assign_genre(
            200,
            GenreOn::Recording(rec()),
            "Techno",
            Source::Discogs,
            Confidence::Matched,
        );
        i.assign_genre(
            300,
            GenreOn::Recording(rec()),
            "Hardvapour",
            Source::Manual,
            Confidence::Confirmed,
        );

        let cat = i.catalogue();
        assert!(
            cat.genres.contains_key(&(
                GenreOn::Recording(rec()),
                Id::<Genre>::of_name("Hardvapour"),
                Source::Manual
            )),
            "the assignment was refused for want of a place in the tree"
        );
        assert_eq!(
            cat.unplaced_genres(browse),
            [Id::<Genre>::of_name("Hardvapour")].into_iter().collect(),
            "the review query is what turns an orphan into a backlog item"
        );
    }
}

#[cfg(test)]
mod genre_retraction_tests {
    //! **One retraction rule, two vocabularies.**
    //!
    //! `GenreRetracted` and `QualifierRetracted` fold through the same
    //! function, so these test the wiring rather than the rule — the rule's
    //! five cases live beside the qualifier facts.

    use crate::model::entity::Genre;
    use crate::model::id::Id;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::reference::Source;
    use crate::model::vocabulary::GenreOn;
    use crate::model::{basis::Basis, spine::Recording};

    fn rec() -> Id<Recording> {
        Id::of_basis(&Basis::External {
            authority: "test".into(),
            key: "callisto".into(),
        })
    }

    fn on() -> GenreOn {
        GenreOn::Recording(rec())
    }

    /// A retraction silences every asserter, and leaves a row saying it was
    /// decided — the state a review queue needs and an absence cannot express.
    #[test]
    fn a_genre_retraction_silences_every_asserter_and_says_so() {
        let mut i = Instance::new(Id::from_uuid(uuid::Uuid::from_u128(1)));
        i.assign_genre(100, on(), "Techno", Source::Discogs, Confidence::Matched);
        i.assign_genre(
            200,
            on(),
            "Techno",
            Source::TagVariant,
            Confidence::Provisional,
        );
        assert_eq!(i.catalogue().genres.len(), 2);

        i.retract_genre(300, on(), "Techno");
        let cat = i.catalogue();
        assert!(
            cat.genres.is_empty(),
            "one asserter survived the retraction"
        );
        assert!(
            cat.genres_retracted
                .contains(&(on(), Id::<Genre>::of_name("Techno"))),
            "the decision left no row, so it reads as though nobody had said"
        );

        // And it is a decision rather than a tombstone.
        i.assign_genre(400, on(), "Techno", Source::Manual, Confidence::Confirmed);
        assert_eq!(i.catalogue().genres.len(), 1);
        assert!(i.catalogue().genres_retracted.is_empty());
    }
}

#[cfg(test)]
mod hierarchy_retraction_tests {
    //! **A wrong edge has to be withdrawable.**
    //!
    //! Without this the browse tree was append-only: `Techno` filed under
    //! `Rock` stayed there, `genre+ = Rock` kept returning techno for good,
    //! and the only repair was to stop using the hierarchy.
    //!
    //! The rule is the one every retraction here uses — shadow, do not delete
    //! — and the third caller for it. The other two are genre and qualifier
    //! assignments; those carry a basis in their live key and so retract by
    //! sweeping, where an edge names no asserter and is one row.

    use crate::model::entity::{Genre, Hierarchy};
    use crate::model::id::Id;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::reference::Source;
    use crate::model::vocabulary::GenreOn;
    use crate::model::{basis::Basis, spine::Recording};

    fn browse() -> Id<Hierarchy> {
        Id::derived(b"browse")
    }

    fn edge(parent: &str, child: &str) -> (Id<Hierarchy>, Id<Genre>, Id<Genre>) {
        (
            browse(),
            Id::<Genre>::of_name(parent),
            Id::<Genre>::of_name(child),
        )
    }

    fn instance(n: u128) -> Instance {
        Instance::new(Id::from_uuid(uuid::Uuid::from_u128(n)))
    }

    /// The edge goes, and a row stays saying somebody decided it should.
    #[test]
    fn a_withdrawn_edge_leaves_a_decision_behind() {
        let mut i = instance(1);
        i.narrows(100, browse(), "Rock", "Techno");
        assert!(i.catalogue().narrows.contains(&edge("Rock", "Techno")));

        i.retract_narrowing(200, browse(), "Rock", "Techno");
        let cat = i.catalogue();
        assert!(!cat.narrows.contains(&edge("Rock", "Techno")));
        assert!(
            cat.narrows_retracted.contains(&edge("Rock", "Techno")),
            "the decision left no row, so it reads as though nobody had filed it"
        );
        assert!(
            !cat.narrows_retracted
                .contains(&edge("Electronic", "Techno")),
            "an edge nobody has ruled on has no row at all"
        );
    }

    /// **The correction survives the sync**, which is the whole reason a
    /// retraction is a fact rather than a delete. A curator withdraws an edge,
    /// and a replica that still holds the original sends it back.
    #[test]
    fn a_retracted_edge_is_not_restored_by_a_replica() {
        let mut mine = instance(1);
        mine.narrows(100, browse(), "Rock", "Techno");
        let stale = mine.records().clone();

        mine.retract_narrowing(200, browse(), "Rock", "Techno");
        mine.merge(300, &stale);
        assert!(
            !mine.catalogue().narrows.contains(&edge("Rock", "Techno")),
            "a stale replica restored a withdrawn edge"
        );
    }

    /// And filing it again works, or a retraction is permanent — which is a
    /// delete wearing another name, and would make one mis-click final.
    #[test]
    fn filing_an_edge_again_revives_it() {
        let mut i = instance(1);
        i.narrows(100, browse(), "Electronic", "Techno");
        i.retract_narrowing(200, browse(), "Electronic", "Techno");
        i.narrows(300, browse(), "Electronic", "Techno");

        let cat = i.catalogue();
        assert!(cat.narrows.contains(&edge("Electronic", "Techno")));
        assert!(
            cat.narrows_retracted.is_empty(),
            "the pair still reads as ruled against while the edge stands"
        );
    }

    /// **Withdrawing the only edge puts the concept back in the backlog.**
    ///
    /// The two halves have to agree: `unplaced_genres` reads the live edges,
    /// so a retraction that left the edge in place would quietly keep a
    /// concept out of the review queue that nothing can reach any more.
    #[test]
    fn withdrawing_the_last_edge_returns_a_concept_to_the_backlog() {
        let rec = Id::<Recording>::of_basis(&Basis::External {
            authority: "test".into(),
            key: "callisto".into(),
        });
        let mut i = instance(1);
        i.narrows(100, browse(), "Electronic", "Techno");
        i.assign_genre(
            200,
            GenreOn::Recording(rec),
            "Techno",
            Source::Discogs,
            Confidence::Matched,
        );
        assert!(
            i.catalogue().unplaced_genres(browse()).is_empty(),
            "a placed concept is not a backlog item"
        );

        i.retract_narrowing(300, browse(), "Electronic", "Techno");
        assert_eq!(
            i.catalogue().unplaced_genres(browse()),
            [Id::<Genre>::of_name("Techno")].into_iter().collect(),
            "the concept is unreachable by closure and nothing said so"
        );
    }
}
